# Shape-aware resolution of active-pattern arguments in pattern position

> **Status:** design + implementation plan. Not started. Closes the pre-existing
> limitation documented at `crates/sema/src/resolve/types.rs:286-295` (the
> `define_active_pattern` doc-comment) and flagged as codex finding 3a during the
> `stage-1-sema-classification` review — see memory
> `sema-resolver-name-resolution-correctness` point 6(d)/(13).
>
> **Revised** after a review that checked the characterization against FCS's
> actual checker (`TcPatLongIdentActivePatternCase`,
> `../fsharp/src/Compiler/Checking/Expressions/CheckExpressions.fs:5263-5392`)
> and re-probed with `dotnet fsi`. Two findings forced changes: (1) the original
> uniform positional rule ("the first `p` args are parameters") is **wrong for
> total single-case recognizers** — FCS splits those `frontAndBack` (the last
> arg is always the result), independent of arity, so partial application of
> the parameters binds a result; the original rule would have *excluded a
> genuine binder*, a regression over today. (2) One probe row (`Lt threshold`)
> pinned an **FS0722-illegal** program via error-tolerant `fcs-dump` — exactly
> the trap memory warns about. The split is now keyed on recognizer *shape*
> (total/partial × single/multi-case), with arity consulted only where FCS
> consults it.

Implement this plan with each stage on its own branch, stacked as necessary on
previous branches, so that a reviewer can review each branch in isolation.

## The problem

An active-pattern **use** in a pattern position — `match n with DivBy divisor`,
`fun (Parse v) -> …`, `let (Parse v) = …` — can carry arguments after the case
name. Some of those arguments are **parameters** (expressions, evaluated in the
enclosing scope) and some are the **result sub-pattern** (a binder). Which is
which depends on the recognizer.

The resolution-independent binder walk (`crates/sema/src/binders.rs`) cannot know
the recognizer, so it treats **every** applied-head argument as a sub-pattern that
binds (`binders.rs:266-273`, recursing `collect` on each arg in the same context).
For a *name* argument this fabricates a definite `DefKind::PatternLocal` binder
(`binders.rs:117-121`), interned with a self-resolution at `exprs.rs:720-725` and
the `bindings.rs` analogues. FCS instead resolves that name to an **outer value**.

Concretely, given `let divisor = 3` and `let (|DivBy|_|) d n = … Some () …`:

```fsharp
match n with DivBy divisor -> …
```

FCS resolves `divisor` to the outer `let divisor` (its declaration is the outer
binding); we fabricate a fresh `PatternLocal` at the `divisor` occurrence. The
classifier then commits `PatternLocal` where FCS reports the value, and
go-to-definition points at the fabricated binder. A *literal* argument (`DivBy 3`)
is already correct — `Pat::Const` binds nothing (`binders.rs:135`); only *name*
arguments are wrong.

The classify differential's declaration-site check (round 3,
`decl_range_agrees` in `crates/sema/tests/all/classify_diff.rs`) already **detects**
this: adding a `DivBy divisor` snippet to the corpus fails the gate on `divisor`
(verified during the round-3 work). The gap is real; it is only left untested in
the corpus because the *fix* needs the recognizer's shape.

## The FCS model (verified against the checker source + probes)

The authoritative split is `TcPatLongIdentActivePatternCase`
(`CheckExpressions.fs:5263-5392`). Given a use `Case a₁ … aₖ` (`k ≥ 1`; `k = 0`
has nothing to split) and the recognizer's **inferred type** stripped to curried
domains `dtys` (`stripFunTy`), with `paramCount = dtys.Length − 1`, the branches
in order:

1. **Bool-returning partial**: `k = paramCount` → all args are parameters, there
   is *never* a result arg; else error FS3868.
2. **Total ∧ single-case ∧ `dtys.Length ≥ k`**: `List.frontAndBack args` —
   parameters = `a₁ … aₖ₋₁`, **result = `aₖ`, always** — *independent of arity*.
   This covers **partial application of the parameters**: with
   `let (|Scale|) k x = k * x` and an outer `let g : int -> int = fun _ -> 999`,
   `match 3 with Scale g -> g 5` **rebinds `g` to the partially-applied
   recognizer** and prints `15`, not `999` (fsi-probed). The original draft's
   positional rule would have classified `g` as a parameter (outer-value use)
   and suppressed the binder — a wrong commit that today's fabricated binder
   gets right.
3. `paramCount = k` → all parameters, provided the case's payload type is unit
   (or an unsolved typar that could be unit); else FS3868.
4. Recognizer's type is an unsolved typar (an active pattern received as a
   *function parameter*, or mutual recursion with a lambda) → `frontAndBack`.
5. `dtys.Length ≠ k` → FS3868.
6. Else (`k = paramCount + 1`) → `frontAndBack`: parameters = `a₁ … aₚ`,
   result = `aₚ₊₁`.

After the split (`CheckExpressions.fs:5389-5390`): non-empty parameters with a
**multi-case** recognizer → **FS0722** "Only active patterns returning exactly
one result may accept arguments". So *parameterized multi-case uses are always
illegal*; the only legal applied multi-case use is the arity-0 payload-carrying
form (`Lt r`, `k = 1` → result binds).

Two consequences the original draft missed:

- The split for **total single-case** heads never consults arity: the last arg
  is the result, everything before it a parameter. (It is therefore also robust
  to eta-reduced definitions, since it does not use the parameter count.)
- **`paramCount` is type-derived, not syntactic.** The syntactic curried count
  is a *lower bound*: eta-reduced recognizers (`let (|DivBy|_|) d = mk d`,
  point-free `let (|P|_|) = f`) undercount. For *partial* recognizers this
  means a use with `k = p_syn + 1` args can mis-bind the last arg exactly as
  today (a residual, documented status-quo unsoundness — **not** a regression);
  the parameters `args[0..p_syn]` are parameters under any true `p ≥ p_syn`,
  so excluding them stays sound.

Evidence (fsi/build-verified where marked; `→` is FCS's classification of the
*argument*):

| recognizer | shape | use | argument resolution |
|---|---|---|---|
| `(\|DivBy\|_\|) d n` (`Some ()`) | partial, single, p=1 | `DivBy divisor` | `divisor` → outer value (**param**; branch 3) |
| `(\|Parse\|_\|) s` (`Some v`) | partial, single, p=0 | `Parse v` | `v` → binder (**result**; branch 6) |
| `(\|DivBy\|_\|) d n` (`Some (n/d)`) | partial, single, p=1 | `DivBy divisor q` | `divisor` → **param**; `q` → **result** (branch 6) |
| `(\|Foo\|) x` | total, single, p=0 | `Foo v` | `v` → binder (**result**; branch 2) |
| `(\|Scale\|) k x` | total, single, p=1 | `Scale factor v` | `factor` → **param**; `v` → **result** (branch 2) |
| `(\|Scale\|) k x` | total, single, p=1 | `Scale g` | `g` → **binder** (partially-applied result; branch 2 — **fsi-probed**, prints 15 with an outer `g` in scope) |
| `(\|Split\|_\|) s` (`Some (a,b)`) | partial, single, p=0 | `Split (a, b)` | `(a,b)` is the result → `a`, `b` bind (compound) |
| `(\|DivBy\|_\|) d n` + `(\|Parse\|_\|) s` | — | `DivBy divisor (Parse v)` | `divisor` → **param**; result `(Parse v)` recurses → `v` binds |
| `(\|Lt\|Ge\|) t n` | total, **multi**, p=1 | `Lt threshold` | **FS0722 — illegal** (fsi-probed). The originally-tabled "threshold → outer" came from `fcs-dump uses-census`, which tolerates type errors. Never add this to a clean corpus. |

The rule holds identically for `match`, `fun`, and `let` patterns (probed with a
`fun` lambda). A quotation-valued parameter (`Foo <@ q @>`) is already handled —
`resolve_pat_types`' `Pat::Quote` arm routes it to `resolve_expr` and the binder
walk's `Pat::Quote` is a no-op (`binders.rs:135`); this is the precedent for how a
*name*-valued parameter should be treated (resolved as an expression, not bound).

> **Probe discipline:** `fcs-dump uses-census` tolerates type errors (see memory
> `sema-resolver-name-resolution-correctness`). Every shape pinned here or added
> to a corpus must also be verified *legal* with `dotnet build` / `dotnet fsi`.

## Current code — leverage points (verified)

- **The fabricated binder** is produced at `binders.rs:266-273` (applied-head arg
  recursion) and interned as a non-provisional `PatternLocal` at
  `exprs.rs:720-725` (match / `fun` / `let!` / `for-in`, via `pattern_locals`,
  `exprs.rs:698-729`) and at `bindings.rs:118-157` / `:229-243` (module / local
  `let`). The binder walk is resolution-independent by design
  (`binders.rs:32-56`) and must stay so. These are the **only three**
  `binders()` call sites in the resolver.
- **Ordering works in our favour**: all three sites run `resolve_pat_types`
  *before* their binder-interning loop (`exprs.rs:708`→`709`,
  `bindings.rs:99`→`111`, `bindings.rs:213`→`225`), and a match clause's frame is
  pushed only after `pattern_locals` returns (`exprs.rs:683-684`). So decisions
  made during `resolve_pat_types` can drive the interning loop, and parameter
  expressions resolve against the *enclosing* scope — the clause's own binders
  are not yet visible, exactly matching FCS evaluation (the `Pat::Quote` arm's
  doc-comment, `types.rs:997-1009`, already relies on this).
- **The applied head is resolved** in `resolve_pat_types`, `Pat::LongIdent` arm,
  `types.rs:971-981`: an applied single-segment head is resolved via
  `case_reference(name)` (`lookup.rs:1507`), which returns `Resolution::Local(use_id)`
  for a same-file active-pattern case (the per-case use def, `DefKind::ActivePattern`,
  ranged at the recognizer span — `types.rs:333-345`). The arm then recurses
  `resolve_pat_types` into each arg (`types.rs:986-995`) for *type annotations and
  nested heads* — not the arg names.
- **Recognizer interning** — `define_active_pattern` (`types.rs:296-363`) takes
  only the `ActivePatName` (`crates/cst/src/syntax/mod.rs:2046-2079`, exposes
  `case_tokens` / `name_range` — **no** parameters). The curried parameter count is
  available **at the call sites** (`bindings.rs:108-110`, `:221-224`) from the head
  `LongIdentPat.args()`, but `active_pat_name_of` (`resolve.rs:482-488`) discards
  everything but the banana name. Totality and case count are visible on the
  `ActivePatName` itself (the case tokens; a trailing `_`).
- **No shape is stored anywhere.** `type_cases` (`state.rs:491`) is union/enum only;
  `DeclKinds` (`state.rs:292-330`) and `ScopeEntry` (`state.rs:20-73`) hold only
  booleans; case entries are keyed by name and resolve to `Resolution::Local(use_id)`.
- **The `Resolver` is per-file** (`resolve.rs:88` constructs one per
  `resolve_file`), so a never-cleared byte-range set is sound.
- **Binder `Def` ranges are ident-*token* ranges** (`Def::from_token`), not
  pattern-node ranges — `DivBy (divisor)` has a `Paren` node range that matches
  no binder. The exclusion mechanics below must key on the walk's own ranges.
- **Cross-file / assembly recognizers carry no shape** — assembly AP tags fold to
  `OpenFoldTarget::Opaque` cases (`assembly_env.rs:1748-1763`), and cross-file
  project AP cases export only `is_case` (`model.rs:40,447`). Neither records the
  parameter count, nor even distinguishes an active pattern from a union case at
  the boundary. So a shape-aware fix is sound only for **same-file** recognizers;
  the cross-file case is a separate slice (Stage 3).

## Design

### Where the shape comes from and lives

`define_active_pattern` gains an `arity: Option<usize>` parameter and computes
the rest of the shape from the `ActivePatName` it already receives:

```rust
// crates/sema/src/resolve/state.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ActivePatternShape {
    /// No trailing `|_|` case.
    pub total: bool,
    /// Exactly one case ident.
    pub single_case: bool,
    /// Curried-parameter count − 1 of the function-form definition
    /// (`let (|DivBy|_|) d n = …` → `Some(1)`). `None` for the bare-name
    /// (point-free) form `let (|P|_|) = …`, whose parameter count is
    /// syntactically invisible (FCS derives it from the inferred type).
    pub arity: Option<usize>,
}
pub(super) active_pattern_shape: HashMap<DefId, ActivePatternShape>,
```

Call sites: the function form (`Pat::LongIdent`, `bindings.rs:108-110`, `:221-224`)
passes `head.args().count().checked_sub(1)`; the bare-name form (`Pat::Named`)
passes `None` — **not** `Some(0)`: a point-free recognizer's parameter count is a
guess, and `None` degrades to today's behaviour. (For a *total single-case*
point-free recognizer the split below never consults arity, so it still applies.)

For each case, `define_active_pattern` inserts
`active_pattern_shape[use_id] = shape` (all cases of one recognizer share it). A
same-file applied head resolved to `Resolution::Local(use_id)` looks up its shape
here; anything else (cross-file `Item`, opaque assembly `Deferred`, qualified
path) is *absent* → shape unknown → unchanged behaviour.

Keying by the resolved def id (not the case-name string) is deliberate: it inherits
`case_reference`'s scoping/shadowing for free — the shape found is exactly the shape
of the recognizer the head actually resolves to.

### Splitting the arguments (same-file, known shape)

In `resolve_pat_types`' applied `Pat::LongIdent` arm, once the head resolves to a
same-file active pattern with a stored shape, with `k = args.count() ≥ 1`:

- **multi-case** → **no split, unchanged behaviour**. Parameterized multi-case
  uses are FS0722-illegal, and the only legal applied use (`k = 1`, arity-0
  payload) already binds correctly today.
- **total ∧ single-case** → parameters = `args[0..k−1]`, result = `args[k−1]`.
  (Arity never consulted; `Scale g` binds `g`.)
- **partial ∧ single-case ∧ `arity == Some(p)`**:
  - `k = p` → all parameters, no result.
  - `k = p + 1` → parameters = `args[0..p]`, result = `args[p]`.
  - `k < p` → FS3868-illegal; treat the present args as parameters (exclude +
    resolve as exprs), never fabricate. Sound on clean code (which cannot reach
    this), conservative on broken code.
  - `k > p + 1` → parameters = `args[0..p]` (sound under eta-reduction too,
    since `p_syn ≤ p_type`); recurse the remaining args as today (FCS errors
    here unless the definition is eta-reduced; status quo either way).
- **partial ∧ `arity == None`** → no split (unchanged behaviour).

**Parameter arguments** — for each:

1. **Exclude its would-be binders**: run `binders(&arg, BinderRole::Pattern)` and
   insert **each returned def's range** into a resolver set
   `excluded_param_ranges: HashSet<TextRange>`. Not the arg *node's* range — the
   walk's `Def` ranges are ident-token ranges (`DivBy (divisor)` would silently
   fail to suppress the binder if the `Paren` node range were used). The role
   only affects `DefKind`s, never ranges, so any role serves.
2. **Resolve it as an expression** via a new helper `resolve_pattern_arg_as_expr`,
   mirroring FCS's `ConvSynPatToSynExpr`: `Named` → value lookup + `record`
   (the outer-value resolution); `Paren` → recurse; `Typed` → resolve the type,
   recurse the inner pattern; `Quote` → `resolve_expr` on the inner (preserving
   today's `Pat::Quote` routing, which the split now bypasses for parameter
   args); `Const` / `Null` / `Wildcard` → nothing; anything else (tuples,
   applications, records, …) → **decline** (record nothing — the exclusion in
   step 1 already prevents fabricated binders; a decline is a coverage gap,
   never a wrong commit). Do **not** recurse `resolve_pat_types` into a
   parameter argument: its `LongIdent` arm resolves applied heads through the
   *constructor* namespace (`case_reference`), which is wrong in expression
   position (`DivBy (Foo x)`: `Foo` is a function/value expression use).

**Result sub-pattern**: recurse `resolve_pat_types` into it as today (it binds;
it may be compound or a nested applied head, which re-enters this same logic).

The binder walk still runs unchanged and still *produces* binders for the
parameter arguments; the three binder-interning loops (`pattern_locals`,
`prepare_binding`, `resolve_local_let`) gain a guard that **skips any def whose
range is in `excluded_param_ranges`** — checked **before** the `provisional`
branch, so a would-be provisional binder (`DivBy Foo`) is dropped rather than
resolved as a case reference. So the parameter arguments end up with the
expression-use resolution recorded in step 2 and *no* fabricated binder (and no
scope entry — the arm body's uses of the name correctly reach the outer value);
the result sub-pattern binds as before.

`excluded_param_ranges` is file-lifetime and never needs clearing: the `Resolver`
is per-file, byte ranges are unique within a file, and a range excluded as a
parameter is never a legitimate binder elsewhere.

This keeps `binders` resolution-independent (unchanged) and confines the
resolution-derived decision to the resolver, exactly as the AP-body decline
(`ap_body_case_names`, round 4) already does.

### Degrade for unknown shape

When the applied head is an active pattern but its shape is **unknown** — a
cross-file `Resolution::Item` case, an opaque assembly case, or a
deferred/qualified head — we cannot tell a parameter from a result. The sound
choice (correctness over availability) is to **decline**: do not fabricate
binders for the head's *name* arguments (add them to `excluded_param_ranges`),
and resolve them as expression value uses where possible. This is Stage 3, and
it is gated on being able to tell a cross-file *active pattern* apart from a
cross-file *union case* (a union case's arguments genuinely bind) — see Stage
3's dependency note.

Stages 1–2 change **nothing** for unknown-shape heads (they keep today's
behaviour), so they are a strict improvement over the status quo; Stage 3
tightens the remaining cross-file gap.

### Soundness notes / edge cases

- **Total single-case, `k ≤ p`** (`Scale g` — partial application of the
  parameters): the last arg **binds** (the partially-applied recognizer's
  result). frontAndBack handles it; the original positional rule was wrong
  here. Must be in the oracle.
- **Partial, `k < p`**: FS3868-illegal; decline the present args as parameters
  (resolve as exprs, no binders). Sound.
- **Partial, `k > p + 1`**: FS3868-illegal (unless eta-reduced, when the true
  `p` is larger); `args[0..p]` are parameters either way — exclude and resolve
  them; recurse the surplus without special-casing (binding there is no worse
  than today).
- **Nullary total case** (`Even`, no args): `k = 0` — nothing to split;
  unchanged.
- **Result sub-pattern is a nested applied AP head** (`DivBy divisor (Parse v)`):
  handled by the result recursion — the inner head re-enters the same logic.
- **Uppercase parameter name** (`DivBy Foo`, `Foo` a value): resolve as an
  expression value use; the exclusion guard runs before the `provisional`
  branch, so the would-be provisional binder is dropped, not case-resolved.
- **Tuple-shaped parameter** (`DivBy (a, b)` for a 2-parameter recognizer): the
  helper declines (records nothing); the exclusion prevents fabricated `a`/`b`
  binders. FCS resolves them as outer values — a decline is a coverage gap,
  never a wrong commit.
- **Eta-reduced / point-free recognizers**: syntactic arity undercounts
  (`None` for point-free). Total single-case splits correctly regardless;
  partial ones keep today's fabricated-binder behaviour on the last arg at
  `k = p_syn + 1` (residual status-quo unsoundness, documented above — do not
  add such shapes to the clean classify corpus).
- **Multi-case heads**: unchanged behaviour everywhere (see the split rule).

## Implementation plan

### Stage 1: Compute and store the recognizer shape (same-file)

**Dependencies**: none.

**Implements**: "Where the shape comes from and lives".

Add `ActivePatternShape` and `Resolver::active_pattern_shape`; thread
`arity: Option<usize>` into `define_active_pattern` from both call sites
(`bindings.rs:108-110`, `:221-224` — `Some(count − 1)` for the function form,
`None` for the bare-name form); compute `total` / `single_case` from the
`ActivePatName` inside `define_active_pattern`. Add a test-only accessor on
`ResolvedFile` — `active_pattern_shape(res: Resolution) -> Option<ActivePatternShape>`
(mirroring `resolved_def`) — so the stored value is observable. No consumption
yet; no behaviour change.

**Correctness oracle** (FCS-free, `resolve_active_patterns.rs`):
- Stored shapes: `(|Even|Odd|) n` → total, multi, `Some(0)`;
  `(|DivBy|_|) d n` → partial, single, `Some(1)`; `(|Scale|) k x` → total,
  single, `Some(1)`; `(|Parse|_|) s` → partial, single, `Some(0)`; a
  three-param `(|P|_|) a b n` → `Some(2)`; a point-free
  `let (|Nil|Cons|) = f` → `None` arity.
- The full existing `borzoi-sema` suite stays green (no behaviour change).

---

### Stage 2: Shape-keyed split for same-file applied heads

**Dependencies**: Stage 1.

**Implements**: "Splitting the arguments" and the legal rows of the FCS table.

Add `resolve_pattern_arg_as_expr` and `Resolver::excluded_param_ranges`; apply
the shape-keyed split in `resolve_pat_types`' applied `Pat::LongIdent` arm;
exclude parameter-arg binder ranges via `binders(&arg, _)`; add the skip guard
(before the `provisional` branch) to the three binder-interning loops. Add the
same-file parameterized shapes to the `classify_diff` corpus (they exercise the
round-3 decl-range and round-5 converse checks, which fail today and pass after
this stage) — **only FCS-legal shapes**: `DivBy divisor`, `DivBy divisor q`,
`DivBy divisor (Parse v)`, `Scale factor v`, and `Scale g`. **Not**
`Lt threshold` (FS0722-illegal — it would fail the gate as an erroring source,
not as a divergence).

**Correctness oracle**:
- Direct (`resolve_active_patterns.rs`, FCS-free): in `let divisor = 3; let
  (|DivBy|_|) d n = … Some() …; match n with DivBy divisor -> …`, the `divisor`
  argument resolves to the **outer value** (`resolved_def(divisor-use).range` is the
  `let divisor`, kind `Value`), not a `PatternLocal` self-binder. For `DivBy divisor
  q`: `divisor` → outer value, `q` → a fresh `PatternLocal` binder. For
  `DivBy divisor (Parse v)`: `divisor` → outer value, `v` → binder. For
  **`Scale g`** (total single-case, `k = p`): `g` → a fresh `PatternLocal`
  binder at its own range, **not** an outer `g` value (pins the frontAndBack
  branch — the original draft's regression case). For `DivBy (divisor)`
  (parenthesised parameter): outer-value resolution and no fabricated binder
  (pins the token-range exclusion keying). For a tuple-shaped parameter: no
  binder, no commit (decline).
- Arm-body scoping: in `match n with DivBy divisor -> divisor`, the body's
  `divisor` resolves to the outer value (the skipped binder must not leave a
  scope entry).
- Regression: `partial_active_pattern_case_use_resolves_and_binds_payload` (`Parse
  v` → `v` binds) and `parameterized_active_pattern_head_resolves_and_binds_args`
  (`DivBy 3` literal) still pass unchanged.
- FCS differential (`classify_diff`): the new parameterized-argument snippets pass
  the soundness gate, the `decl_range_agrees` check, and the converse check — i.e.
  the `divisor` arguments now classify compatibly with FCS's outer-value symbol
  (they would fail before this stage). Every added snippet must be
  `dotnet build`/fsi-verified legal first.
- The full `borzoi-sema` suite and the whole-project `resolve_corpus_diff` /
  `classify_diff` gates stay green.

---

### Stage 3 (optional follow-up): degrade unknown-shape heads

**Dependencies**: Stage 2. **May be deferred** — it is a *separate* pre-existing
gap (cross-file active patterns are barely modelled) and requires new cross-file
metadata.

**Implements**: "Degrade for unknown shape".

For an applied head that resolves to an active pattern whose shape we cannot see
(cross-file `Item`, opaque assembly case, deferred/qualified head), decline its
name arguments rather than fabricate binders. **Blocked on** being able to tell a
cross-file *active pattern* apart from a cross-file *union case* at the use site —
today the boundary records only `is_case` (`model.rs:447`; `assembly_env.rs`
opaque tags), and a union case's arguments genuinely bind, so a blanket decline
would regress `Some x`-style cross-file union patterns. The enabling infrastructure
is exporting the case *kind* (and ideally the shape) across the Compile-order /
assembly boundary — a prerequisite sub-stage.

**Correctness oracle**:
- A cross-file / opened parameterized active pattern used as `Foo bar` no longer
  fabricates a `PatternLocal` for `bar` (it declines); a cross-file *union* case
  `Some x` still binds `x`.
- FCS differential over a multi-file fixture exercising both: certain-implies-agree
  holds; no fabricated-binder commitment survives.

## References

- The authoritative FCS split: `TcPatLongIdentActivePatternCase`,
  `../fsharp/src/Compiler/Checking/Expressions/CheckExpressions.fs:5263-5392`
  (branch structure at `:5350-5387`, the multi-case FS0722 post-check at
  `:5389-5390`). Error codes: FS0722 (`tcRequireActivePatternWithOneResult`),
  FS3868 (`tcActivePatternArgsCountNotMatch*`) — `FSComp.txt:578,1777-1780`.
- Pre-existing limitation doc-comment: `crates/sema/src/resolve/types.rs:286-295`.
- Binder walk applied-head arm: `crates/sema/src/binders.rs:226-285`.
- Applied-head resolution: `crates/sema/src/resolve/types.rs:946-996`.
- Pattern-binder engines: `pattern_locals` (`crates/sema/src/resolve/exprs.rs:698-729`),
  `prepare_binding` / `resolve_local_let` (`crates/sema/src/resolve/bindings.rs`).
- `case_reference`: `crates/sema/src/resolve/lookup.rs:1507`.
- The classify differential the fix must satisfy: `crates/sema/tests/all/classify_diff.rs`
  (`decl_range_agrees`, the converse loop).
- Memory: `sema-resolver-name-resolution-correctness` points 6(d) and 13, and its
  fcs-dump-tolerates-errors warning (which bit the original draft's `Lt threshold`
  row).
