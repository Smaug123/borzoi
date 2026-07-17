# Parser plan

> **Status (2026-07-15).** Phases 1–10 complete: the parser produces a faithful
> tree over the whole valid-input surface (see **Current state** for the
> one-line-per-phase list). **Phase 11 — error recovery** is in progress
> (`NormalisedExpr::Error` recovery marker landed for the incomplete-`let`-RHS,
> trailing-body, keyword-aware-`if`, incomplete-`match`/`function`/`try`, and
> incomplete-lambda slices), and a handful of long-tail slices remain — see
> **Phase 11** and **Open slices**. Per-feature histories live in the code and
> git log, not here.

Design doc for the parser stream that consumes `crates/cst/src/lexfilter`'s
output and produces a tree mirroring FCS's `ParsedInput`. Captures the decisions
made before implementation started so future work can resume from a cold pickup.

## Scope

- **Input.** `Iterator<FilteredToken>` from `crates/cst/src/lexfilter` (already
  differential-tested against FCS's post-`UseLexFilter` token stream).
- **Output.** A tree whose typed-AST view mirrors FCS's `ParsedInput`
  (`ImplFile | SigFile`), with full range information on every node.
- **Errors.** Phase 11 recovery is in progress. Several expression-hole cases
  recover to partial trees with zero-width `ERROR` placeholders plus
  `ParseError`s; broader delimiter and statement-level resync remains.
- **Reference.** `../fsharp/src/Compiler/pars.fsy` (yacc grammar) and
  `../fsharp/src/Compiler/SyntaxTree/SyntaxTree.fsi` (AST shape). pars.fsy is
  documentation, not a porting target — yacc semantic actions don't translate
  cleanly to Rust.

## Settled decisions

### D1. Parser style: hand-written recursive descent

Rationale: F#'s grammar has context sensitivity that LexFilter only partially
resolves; declarative LALR/PEG generators would push the disambiguation into
semantic actions anyway. Hand-written RD also makes per-feature incremental
porting straightforward and gives clean local reasoning (one function per
production).

Alternatives rejected: **lalrpop / chumsky** (same disambiguation work inside
semantic actions, no net win); **mechanical port of pars.fsy** (yacc actions are
coupled to fsyacc's parsing stack and `ParseErrorContext`; the output is
unreadable).

### D2. AST shape: mirror FCS variants

`SynExpr::App`, `SynPat::Named`, etc. — variant names and field order match
FCS one-to-one. Maximises diff-harness leverage. Deviations are allowed
locally when a variant is unambiguously equivalent (e.g. collapsing a
deprecated case) but require a comment.

### D3. Tree representation: rowan green/red, using the `rowan` crate

User priorities: trivia retention + LSP-grade speed; willing to pay complexity.

What rowan gives: **first-class trivia** (whitespace, comments, every keyword
token are children of the green tree); **incremental reparse** (green nodes are
structurally hashed and shared, so editing one token re-parses one local
subtree); **lazy red layer** (parent pointers / absolute offsets computed on
demand); **battle-tested** (rust-analyzer uses it at production scale).

Costs accepted: up-front scaffolding (flat `SyntaxKind` enum,
`GreenNodeBuilder`-driven parser API, typed-AST facade layer);
`match expr.kind() { … }` + `ast::Expr::cast` instead of `match expr { … }`; a
hand-maintained typed-AST facade (revisit `ungrammar` codegen once the AST
stabilises).

Alternatives rejected: **Box-recursive enums** (no incremental reparse, trivia
becomes an afterthought); **roll our own green/red tree** (speculative until we
hit a concrete perf ceiling rowan can't clear).

### D4. Differential testing: normalised intermediate

Same pattern as `NormalisedToken` in `tests/all/common/mod.rs`. Both sides project
to a shared `NormalisedAst`:

```
                    Rust source
                        |
        +---------------+---------------+
        v                               v
  Our parser:                   FCS ParseFile (via
  GreenNode tree                tools/fcs-dump ast,
        |                       AdjacentTag JSON):
  Typed facade                  Box-recursive
  (ast::Expr view)              ParsedInput
        |                               |
        v                               v
            NormalisedAst (shared)
                    |
                  diff
```

The normaliser elides trivia and ranges by default; individual tests opt in to
pinning them. This makes the rowan-vs-FCS shape difference a non-issue for
testing: the diff never sees rowan internals, only the projected shape.

Alternative rejected: **match FCS's JSON shape byte-for-byte** (less code, but
every internal FCS field becomes part of our contract).

### D5. SyntaxKind naming: mirror FCS variant names

`APP_EXPR`, `LONG_IDENT_PAT`, `LET_DECL`, etc. — direct map from FCS's
`SynExpr.App`, `SynPat.LongIdent`, … Keeps the FCS-side projector trivial.

### D6. Typed-AST facade: hand-written initially

Per-feature, mechanical. Matches the incremental-port style used for the lexer
and lexfilter. Revisit `ungrammar` codegen once the burden is felt.

### D7. Trivia: retained as first-class green-tree children

Whitespace, comments, every keyword token live in the green tree. Diff harness
elides them in the default normaliser; specific tests opt in to pinning ranges
when that's what's under test.

### D8. Source positions: rowan-native byte offsets

Rowan tracks byte offsets natively. Line/col derived from a `LineMap` only when
crossing the FCS boundary (for diff) or surfacing to LSP.

## Working conventions

One PR per (sub-)phase, each adding: `SyntaxKind`s, parser productions,
typed-AST accessors, the `NormalisedAst` projector on **both** sides, and a
`parser_diff` test pinning the FCS-equivalent shape (the .NET SDK is on `PATH`
in CI and the devshell, so these run by default). Mirrors the lexer/lexfilter
cadence.

**The correctness oracle throughout is the differential harness.**
`assert_asts_match` (`tests/all/common/mod.rs`) projects our CST and FCS's
`ParsedInput` to the shared `NormalisedAst` and asserts equality (plus that
neither side errored); `assert_asts_match_allow_errors` is the recovery-friendly
variant. **Ground-truth every shape with `dotnet tools/fcs-dump ast <file.fs>`
before coding** — never infer a shape from pars.fsy.

Source layout: the parser lives in `crates/cst/src/parser/` (split by concern —
`decls*.rs`, `expr*.rs`, `pat.rs`, `types.rs`, `classify.rs`, `cursor.rs`, …);
the `SyntaxKind` enum and typed facade in `crates/cst/src/syntax/`
(`kinds.rs`, `mod.rs`); the normaliser in
`crates/cst/tests/all/common/normalised_ast/` (`model.rs`, `from_cst.rs`,
`from_fcs.rs`); diff tests in `crates/cst/tests/all/parser_diff_*.rs`; sema in
`crates/sema/src/resolve.rs`.

Workflow to add a feature: write the failing `assert_asts_match` test(s) first,
confirm they fail, implement, re-run. To see *why* a case diverges, dump our
side with `borzoi_cst::parser::parse(src).root` (`{:#?}`) and the filtered
token stream with
`borzoi_cst::lexfilter::filter(src, borzoi_cst::lexer::lex(src))`.

## Current state

All of phases 1–10 are complete. One line per phase:

1. **Scaffold + smallest valid input** — `SyntaxKind` enum, builder-driven
   cursor, `NormalisedAst` projector, end-to-end harness.
2. **Literals, idents, `SynLongIdent`** — all `SynConst` variants; dotted paths.
3. **Atomic expressions + infix operators** — parens, tuples, application,
   Pratt-climbing infix precedence, prefix operators.
4. **`let` bindings** — top-level and `let … in`; `let rec`/`and` chains;
   `inline`/`mutable`; function-form; wildcard heads; virtual layout tokens.
5. **`if`/`elif`/`else`, `fun`-lambdas, `match`/`function`** — `when` guards,
   offside clause bodies, `SynExpr.MatchLambda`, the `SimplePatsOfPat` lowering
   for `fun`-args (`_argN` counter matching FCS's `SynArgNameGenerator`).
6. **Patterns (`SynPat`)** — every in-scope variant; long-ident heads with
   dotted paths; struct tuple patterns; the precedence-climbing infix tail.
7. **Types (`SynType`)** — every variant reachable from the typed-paren surface;
   long-tail variants folded into their phase-9/10 consumers.
8. **Module/namespace structure** — `open` (incl. `open type`), named-module /
   namespace / global-namespace headers (with `rec` and access modifiers),
   multiple namespaces per file, nested modules, module abbreviation,
   `begin … end`-wrapped module bodies.
9. **Type definitions** — Block A (carrier/header/abbreviation, `and`-chains,
   type parameters with `when` constraints, records, unions, enums), Block B
   (the full object model: members, implicit/explicit ctors, `val` fields,
   auto-properties, `override`/`default`/`abstract`, `inherit`, `interface`,
   explicit kind markers, augmentations, get/set), Block C (exceptions).
10. **Long tail** — quotations, computation expressions, `while`/`while!`,
    record / anon-record / struct expressions, attributes (10.5–10.7, all
    carriers), units of measure (10.8), type-provider static args (10.9),
    `SynType.Intersection` (10.10), dot-access / indexers / ranges / slices
    (10.16, 10.22), type-app expressions (10.21), list/array literals,
    `try`/`with`/`finally`, object expressions (all forms), `new`,
    inferred and infix coercions (`upcast`/`downcast`, `:>`/`:?>`/`:?`),
    expression cons, block `let`/`use`, and `.fsi` signature files (Block D,
    10.11–10.15, incl. named/optional/typar/literal `val` sigs and member sigs).

For the deliberate ways the parser differs from FCS, see
`docs/fcs-divergences.md`.

---

# Remaining work

## Phase 11 — error recovery (in progress)

Token-level resync (skip to the next likely statement start), partial trees with
`ParserDetail::ErrorRecovery`, and a diagnostic at each error site. Turns the
first-error-bail behaviour into the LSP-grade partial parsing the server wants,
and picks up the deferred `SynPat`/`SynExpr` recovery-placeholder variants parked
by earlier phases.

**Landed slices (one line each).** The recovery *currency* is a shared
`NormalisedExpr::Error` marker (FCS's `SynExpr.ArbitraryAfterError` ≡ our
absent-`Expr`-child zero-width `ERROR`), so the surrounding recovered structure
diffs against FCS via `assert_asts_match_allow_errors`; the following decl still
parses in every case:

- Incomplete `let` binding RHS — `let x =`, `let x`, `let x = =`, `let x = type`,
  `let x : int =` (the annotated hole keeps its `Typed(Error, int)` wrapper)
  (`parser_diff_let_bindings.rs`).
- Incomplete trailing body — a missing `if c then` branch
  (`IfThenElse(c, Error, None)`, `parser_diff_control_flow.rs`) and a missing
  block-`let … in` body (`LetOrUse([z→Error], Error)`, `parser_diff_let_bindings.rs`).
- Keyword-aware `if` branch resolution — branches resolved relative to their
  `THEN_TOK`/`ELSE_TOK` (with a new `has_else()`), so a hole is attributed to the
  correct slot even when not trailing; identical to the old positional result for
  well-formed input including `elif` chains.
- Incomplete `match` / `function` / `try … with` — the clause-list projection
  (`normalise_match_clauses`) drops the spurious empty clause at the
  `with`/`function` boundary and projects a missing clause result to `Error`
  (`parser_diff_match.rs`, `parser_diff_try.rs`).
- Incomplete lambda body — `fun … ->` with no body projects to
  `Lambda(args, Error)` (`normalise_fun`, `parser_diff_functions.rs`).

Next slices, roughly in priority order:
- **Unclosed-delimiter resync** (`let x = (`, `f (`, `{`) — a *parser*-side
  slice, not normaliser-only: the open delimiter suppresses the offside rule, so
  the rest of the file is swallowed as `ERROR` tokens inside the binding and the
  following decl is lost (FCS keeps it). Needs the parser to resync the
  unclosed delimiter at an offside decl boundary. (`{` also diverges in kind —
  FCS recovers a `Record`, we make a `ComputationExpr`.)
- *FCS*-side unmodeled recovery cases — unclosed list and dangling dot hit
  `from_fcs` cases the projector doesn't model (`FromParseError` /
  `DiscardAfterMissingQualificationAfterDot`); a lambda body with a trailing
  `| …` also lands here.
- **Statement-level resync in the module-decls loop** — when a top-level decl
  fails, skip to the next `let`/`type`/`module`/`open`/`namespace` at-or-left of
  the module column, emit an `ERROR` node for the bad span, and continue. (The
  loop already skips-one-token-and-continues losslessly; this is about matching
  FCS's *resync point* so the recovered tree diffs.)
- `SynPat` recovery placeholders (the deferred `FromParseError` pattern arm).

Note a deliberate non-goal: where FCS's recovery is *worse* than ours (e.g. a
stray top-level `)` makes FCS bail to EOF and drop a valid trailing decl, which
we keep), we do **not** degrade to match it — that is a leniency, not a bug.

## Open slices (small, PR-sized, harness-verifiable)

(Two former open slices have since landed and are no longer open:
`global.`-rooted patterns (#915) and module/class-scope `let … in` (#681).)

- **Non-block CE binders** — `let!`/`use!`/`and!` outside block position
  (`(let! x = m in x)`, `when let! …`, an infix RHS `e + let! …`) arrive as a
  raw `Token::LetBang`/`UseBang` and reject cleanly. Parsing the raw
  `BINDER … IN` form is a follow-up slice. (The non-block plain `let … in`
  operand parses today; the non-block `use … in` operand stays a rare deferred
  divergence — FCS relabels its leading keyword to `Let`, which our text-based
  `is_use` can't reproduce without misrepresenting the source.)
- **Niche type-defn forms** — `SynMemberDefn.NestedType` (`static type …`
  inside a class); the narrow residual `SynTypeConstraint` gap
  `default 'a : t`.
- **`_.M` special-rooted pattern** — FCS's `UNDERSCORE DOT pathOp`, gated on the
  F# 4.7 `SingleUnderscorePattern` feature; rejects cleanly today.
- **`('a,'b) T` PrefixList typar form** (`SynTyparDecls.PrefixList`) — a clean
  error today.

## Known leniencies (well-formed tree, only a missing FCS diagnostic)

These accept input FCS rejects, but produce a lossless, well-formed tree — FCS
produces *no* tree to diverge from, so the only gap is the diagnostic. (The
fuller divergence catalogue is `docs/fcs-divergences.md`.)

- **Repeated sequence separators** — `(1; ; 2)`, `[1; ; 2]`, `let x = a; ; b`;
  `parse_seq_block_body` consumes a maximal run of `;`/`OBLOCKSEP`. Tightening
  the shared gatherer to a single separator group is a cross-cutting follow-up.
  Same gatherer, same leniency: a `;`-separated sibling after a literal-value
  `val` sig (`val x : int = 1; val y : int`) — the trailing `;` lands inside the
  literal RHS block and is absorbed, so the sibling parses, where FCS errors
  (FS0010). Pinned by `sig_val_literal_then_semi_sibling_is_lenient_lossless`.
- **Bare head `*` wildcard** — `let r = *` / `- *` parse as
  `IndexRange(None,None)`; FCS rejects a head `*` on an offside rule we don't
  replicate. Every in-bracket / in-tuple / range-bound / infix use matches FCS.
- **Multiple single-line object-expression interfaces** —
  `{ new T() interface IA with … interface IB with … }`; FCS errors on the
  second interface on a layout rule we don't replicate.
- **`use mutable` / `use rec`** accepted where FCS errors (the modifier path is
  shared with `let`).

## Risks carried forward

- **Precedence table** (`pars.fsy`, ~200 lines of `%left`/`%right`/`%nonassoc`).
  Translated into the expression and pattern Pratt climbers; verifiable by
  property test (feed random well-typed expressions through both sides).
- **Computation expressions.** `pars.fsy` lowers them at parse time via
  `SyntaxTreeOps`; we mirror that so the oracle works.
- **`SynPat::LongIdent` vs `SynPat::Named`** approximates name resolution at
  parse time: multi-segment path **or** uppercase single ident → `LongIdent`;
  bare lowercase single ident → `Named` (`pars.fsy:3810`); function-form heads
  always `LongIdent`. The remaining approximation is purely the leading-case
  test, as FCS itself does.
- **`HIGH_PRECEDENCE_TYAPP`** is emitted by LexFilter and trusted to
  disambiguate `f<int>` from `f < int`; consumed on both sides (type-level
  `APP_TYPE`, expression-level `TYPE_APP_EXPR`). Only emitted when
  `peekAdjacentTypars` finds the matching `>`, so an unclosed `f<int` stays the
  comparison. Lexfilter bugs surface as `parser_diff` failures.
