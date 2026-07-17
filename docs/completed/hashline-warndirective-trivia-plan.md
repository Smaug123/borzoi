# `HASH_LINE` / `WARN_DIRECTIVE` trivia token kinds

> **Status: completed.** Stages A / B1 / B2 all landed — the trivia
> infrastructure (`SyntaxKind::HASH_LINE` / `WARN_DIRECTIVE`, `TriviaToken`,
> the full-trivia driver mode) shipped in PRs #246 / #247 / #250. This
> document planned the "infrastructure-only" half of the deferred follow-up
> sketched in `docs/ifdef-plan.md` → *Deferred follow-ups* → *`HashLine` /
> `WarnDirective` syntax token kinds*. The **green-tree consumer** (making
> the recursive-descent parser ifdef-aware so these tokens actually reach
> the rowan tree that hover / format / semantic-tokens read) was explicitly
> **out of scope** here (see *Out of scope* below) and has since landed too
> — `docs/completed/parser-ifdef-plan.md`.

Implement this plan with each stage on its own branch, stacked as necessary
on previous branches, so that a reviewer can review each branch in
isolation.

## Goal

Teach the preprocessor driver to surface `#line` / `# N "file"` and
`#nowarn` / `#warnon` directives as **trivia tokens** (FCS's `HASH_LINE`
and `WARN_DIRECTIVE`) instead of silently swallowing them, and add the two
corresponding `SyntaxKind` variants. This is the lexer-vocabulary
infrastructure a future full-fidelity tree will splice into the green tree;
it lands the data/driver half now, mirroring exactly how PR #157 landed the
structured-payload *data model* while deferring its consumer.

## Background: why this is "infrastructure only"

There are two independent pipelines in the crate today, and the deferred
slice straddles them:

1. **The green tree** is built by `cst::parser::parse()`
   (`crates/cst/src/parser/mod.rs:57`): `lex()` → `filter()` →
   recursive-descent → rowan `GreenNodeBuilder`. **This pipeline never
   touches the directive driver.** Per `crates/lsp/src/diagnostics.rs:22-26`,
   the parser "has no preprocessor handling yet, so it parses *every* `#if`
   branch and the directive lines themselves." `docs/parser-plan.md` defers
   parser-level hash directives (`SynModuleDecl.HashDirective`) to phase 10
   and never wires `lex_with_symbols` into `parse()`.

2. **The directive driver** `lex_with_symbols`
   (`crates/cst/src/directives/driver.rs:163`) is the ifdef-aware
   tokeniser. It currently **swallows** `#line` / `#nowarn` / `#warnon`
   (`handle_directive_result`, `driver.rs:266-293`): on a recognised trivia
   directive it advances `pos` past the line and emits no token. It is
   consumed only by the LSP diagnostics path (`diagnostics_for`,
   `diagnostics.rs:84`), the corpus tests, `lexer_diff`, and
   `lexfilter_corpus` — none of which build a tree.

So "wire the driver to emit them as trivia tokens" can deliver a
*driver-level* capability today, but those tokens only become
**tree-visible** once the parser is switched to consume the driver — a
substantially larger, currently-unplanned effort. This plan delivers the
driver capability + the `SyntaxKind` vocabulary; the consumer stays
deferred.

## FCS reference

FCS has a single lexer (`src/Compiler/lex.fsl`) parameterised by a `skip`
flag (`rule token (args: LexArgs) (skip: bool)`, lex.fsl:336):

- `skip = true` (compiler / fsi): trivia directives are processed for
  effect and lexing recurses — no token is emitted. **This is what our
  `lex_with_symbols` does today.**
- `skip = false` (editor / VS): an *artificial* trivia token is emitted.

The two directive trivia kinds in scope:

| Source form | lex.fsl | FCS token | maps to |
|---|---|---|---|
| `#line N` / `# N "file"` / `# N @"file"` | lex.fsl:757-811 | `HASH_LINE` | `SyntaxKind::HASH_LINE` |
| `#nowarn …` / `#warnon …` | lex.fsl:1084-1089 | `WARN_DIRECTIVE` | `SyntaxKind::WARN_DIRECTIVE` |

Both are **hidden tokens**: declared in `pars.fsy` (`HASH_LINE` at
pars.fsy:154; `WARN_DIRECTIVE` at pars.fsy:155) but consumed by no grammar
rule — the parser discards them, and `LexFilter.fs` passes them through
untouched. They exist purely so an editor-mode token stream is lossless.

Note FCS *also* emits `HASH_IF` / `HASH_ELSE` / `HASH_ELIF` / `HASH_ENDIF`
for the conditional-compilation directives under `skip=false`. Those are
**out of scope** here (see below), matching the deferred slice, which names
only `HASH_LINE` and `WARN_DIRECTIVE`.

## Design

### Token representation: a `TriviaToken` wrapper, not new `Token` variants

The driver's item type is `(Result<Token<'a>, PreprocError>, Range)`. The
recognised directive is a single line we deliberately *do not* lex (its body
may be malformed by design — e.g. `#line 5 "unterminated`), so we want one
opaque token covering the whole range, not its constituent lexed pieces.

Introduce a small wrapper enum in `crates/cst/src/directives/`:

```rust
/// A token in the full-trivia preprocessor stream: either a real lexer
/// token from an active branch, or a directive-trivia marker for a
/// `#line` / `#nowarn` / `#warnon` line that skip-mode would swallow.
pub enum TriviaToken<'a> {
    Lexed(Token<'a>),
    /// `#line N` / `# N "file"` — FCS `HASH_LINE`.
    HashLine,
    /// `#nowarn …` / `#warnon …` — FCS `WARN_DIRECTIVE`.
    WarnDirective,
}
```

Rationale — *not* adding `Token::HashLine` / `Token::WarnDirective`:

- `Token` is the pure, context-free, `#[derive(Logos)]` lexer enum
  (`crates/cst/src/lexer/mod.rs:146`); the design (AGENTS.md / ifdef-plan.md
  §"Where the directive layer sits") keeps it that way so the lexer-diff
  surface never changes. The directive markers are synthesised by the
  *driver*, never by `lex`; they don't belong on the logos enum (and a
  logos variant carrying no `#[token]`/`#[regex]` is awkward at best).
- Adding `Token` variants would force every exhaustive `match Token` in the
  workspace (the lexfilter, the parser's token→kind dispatch, the
  lexer-diff normaliser) to grow arms for tokens the lexer can never
  produce — churn with no payoff.

The wrapper localises the new surface to the driver and the (future)
consumer, and lets existing consumers keep the unchanged `Token` item type.

### Driver mode: shared core + a `trivia_mode` flag

The state machine in `Driver` is identical between the two modes *except*
at the trivia-directive branch of `handle_directive_result`. Refactor so a
private core produces `TriviaToken<'a>` items and carries a `trivia_mode`
flag:

- At the trivia-directive branch: if `trivia_mode` is off, swallow exactly
  as today (advance `pos`, emit nothing); if on, enqueue one
  `TriviaToken::HashLine` / `WarnDirective` over `Recognised.range` (and
  still record `#line` into the `LineDirectiveStore`, unchanged, so the LSP
  line-remap path keeps working in either mode).
- Active-branch lexer tokens are wrapped `Lexed(token)`; errors stay
  `Err(PreprocError)`; CC-directive handling (`#if`/`#else`/`#elif`/
  `#endif`), EOF, and interp-frame plumbing are unchanged.

Public surface:

- `lex_with_symbols(source, symbols) -> Driver` keeps its **exact** current
  signature and behaviour. `Driver` wraps the core with `trivia_mode =
  false` and its `Iterator::next` maps `Lexed(t) → t` — total, because with
  `trivia_mode` off the core never yields a directive marker. The
  `line_directives()` accessor is preserved.
- `lex_with_symbols_full_trivia(source, symbols) -> FullTriviaDriver`
  (Item `= (Result<TriviaToken<'a>, PreprocError>, Range)`) wraps the core
  with `trivia_mode = true` and also exposes `line_directives()`.

This is the direct analogue of FCS's one-lexer-with-`skip`-flag shape, and
keeps the blast radius inside the driver module: no existing consumer
(`diagnostics_for`, corpus, `lexer_diff`, `lexfilter_corpus`) is touched.

### `SyntaxKind` additions + the bridge

- Add `SyntaxKind::HASH_LINE` and `SyntaxKind::WARN_DIRECTIVE` to the trivia
  cluster (near `BLOCK_COMMENT`, **before** the `__LAST` sentinel, so the
  `from_raw` range-check at `kinds.rs:1248` keeps working — no explicit
  discriminants, no gaps), with doc comments mirroring the FCS rules above.
- Extend `SyntaxKind::is_trivia` (`kinds.rs:1261`) to include both — its
  doc already anticipates "a later full-fidelity pass will splice [trivia]
  into the green tree."
- Provide the driver→kind bridge that the future consumer will call:
  `TriviaToken::directive_kind(&self) -> Option<SyntaxKind>`
  (`HashLine → HASH_LINE`, `WarnDirective → WARN_DIRECTIVE`, `Lexed → None`).
  This is what keeps the new `SyntaxKind` variants from being orphaned: it
  is the single, tested edge connecting the driver's emission to the tree
  vocabulary. (`directives` may import `crate::syntax::SyntaxKind`; the
  dependency is acyclic — `syntax` does not import `directives`.)

### Range / losslessness contract

Emit exactly one trivia token per recognised directive, spanning
`Recognised.range` = `line_start .. line_end` (excluding the line
terminator; `range` already starts at `line_start`, so any leading
horizontal whitespace before the `#` is *inside* the token, matching FCS's
`anywhite*`-prefixed lexeme). The trailing `\n` / `\r\n` is outside the
range and is lexed by the normal stream as a `Newline`, exactly as in
skip-mode. A future tree consumer therefore gets `text(tree) == source`
across a directive line without any special casing. Pin the exact range
(including leading whitespace) in an example test so the contract is
explicit.

## Implementation plan

### Stage A — `SyntaxKind` vocabulary + trivia classification

**Dependencies**: none.

**Implements**: the `SyntaxKind` half of the deferred slice.

- Add `SyntaxKind::HASH_LINE` and `SyntaxKind::WARN_DIRECTIVE` (before
  `__LAST`) with FCS-referencing docs.
- Extend `SyntaxKind::is_trivia` to match them.

**Correctness oracle**:

- `from_raw` round-trips every discriminant in `0..__LAST`, including the
  two new ones (add/extend the round-trip test if one exists; otherwise a
  `for raw in 0..(SyntaxKind::__LAST as u16)` sweep asserting
  `from_raw(raw).is_some()` and `kind_to_raw(from_raw(raw)) == raw`).
- `assert!(SyntaxKind::HASH_LINE.is_trivia() && SyntaxKind::WARN_DIRECTIVE.is_trivia())`.
- `cargo build` / `cargo clippy` clean and
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features` passes
  (the doc comments link to sibling kinds).
- **Verify (checklist):** no *exhaustive, wildcard-free* `match` over
  `SyntaxKind` exists that would fail to compile with the new arms (grep the
  candidate matches in `crates/cst/src/{syntax/mod.rs,parser/mod.rs}`; the
  AST projection/normaliser typically uses a `_` arm, but confirm).

This is the trivial prelude stage — additive vocabulary with the simplest
possible oracle.

### Stage B1 — Driver core refactor (behaviour-preserving)

**Dependencies**: Stage A is *not* required (B1 touches no `SyntaxKind`),
but landing A first keeps the stack linear.

**Implements**: the `TriviaToken` type and the shared-core refactor, with
the new mode **not yet exposed** — `trivia_mode` is hard-wired off.

- Introduce `pub enum TriviaToken<'a>` (as above).
- Extract the `Driver` state machine into a private core whose item type is
  `TriviaToken<'a>` and which carries `trivia_mode: bool` (constructed
  `false`). `Driver` becomes a thin wrapper that unwraps `Lexed(t) → t` and
  re-exposes `line_directives()`. No new public entry point yet.

**Correctness oracle** (pure refactor → "nothing changed"):

- The entire existing suite stays green with **zero edits to existing
  tests**: the `driver.rs` unit tests (incl. the `reference_lex` /
  `collect_line_directives_reference` PBTs and the totality proptest),
  `tests/corpus.rs`, `tests/lexer_diff.rs`, `tests/lexfilter_corpus.rs`.
- `lex_with_symbols`'s public signature and `Driver`'s public API
  (`Iterator<Item = (Result<Token<'a>, PreprocError>, Range)>` +
  `line_directives()`) are unchanged (a doc/`cargo public-api`-style eyeball
  diff, or simply that no caller needed editing).

### Stage B2 — Full-trivia mode + emission + bridge

**Dependencies**: Stages A and B1.

**Implements**: the driver-emission half of the deferred slice and the
driver→`SyntaxKind` bridge.

- Add `pub fn lex_with_symbols_full_trivia(...) -> FullTriviaDriver`
  (`trivia_mode = true`) emitting `TriviaToken::HashLine` /
  `WarnDirective` over `Recognised.range` instead of swallowing; preserve
  `line_directives()`.
- Add `TriviaToken::directive_kind(&self) -> Option<SyntaxKind>` and a
  unit test pinning `HashLine → HASH_LINE`, `WarnDirective → WARN_DIRECTIVE`,
  `Lexed(_) → None`. (This is the edge that makes Stage A's variants live.)
- Re-export the new items from `directives/mod.rs`.

**Correctness oracle**:

- **Additive-equivalence (PBT, primary):** for arbitrary `(source,
  symbols)`, taking `lex_with_symbols_full_trivia(...)`, dropping every
  `HashLine` / `WarnDirective` item, and unwrapping `Lexed` yields a stream
  **token-for-token, span-for-span, error-for-error identical** to
  `lex_with_symbols(...)`. Proves the default path is untouched and the new
  mode only *inserts* trivia tokens.
- **Directive-span correspondence (PBT):** the multiset of `(kind, span)`
  for emitted directive-trivia tokens equals the set of *active-branch*
  trivia-directive ranges computed by an independent reference walk — extend
  the existing `collect_line_directives_reference`
  (`driver.rs:629`) into a `collect_trivia_directives_reference` that also
  enumerates `#nowarn` / `#warnon` ranges. Cross-check kind:
  `HashLine ↔ Directive::Line`, `WarnDirective ↔ Directive::{NoWarn,WarnOn}`.
  (Restricted, like the existing oracle, to inputs without multi-line
  strings / block comments.)
- **Totality (PBT):** `lex_with_symbols_full_trivia` never panics on
  arbitrary input (extend the existing totality proptest harness).
- **Inactive-branch suppression (example):** a trivia directive inside a
  dead `#if` branch is **not** emitted (consistent with skip-mode dropping
  it; the F# compiler never sees it).
- **Example tests** mirroring the existing swallow tests, now asserting
  emission, kind, and *exact range*: `#nowarn "40"`, `#warnon "3218"`,
  `#line 5 "foo.fs"`, bare-numeric `# 1 "fsyacclex.fsl"`, a directive inside
  an *active* `#if`, and a leading-whitespace case (`   #nowarn "40"`)
  pinning the range start at `line_start`.
- Existing consumers stay green (they still call `lex_with_symbols`): full
  `cargo test` across the workspace.

## Out of scope (deferred follow-ons)

- **The green-tree consumer.** Making `cst::parser::parse()` consume the
  directive driver so `HASH_LINE` / `WARN_DIRECTIVE` (and the rest of an
  ifdef-aware stream) actually land in the rowan tree that hover / format /
  semantic-tokens read. This forces decisions about how `#if`/`#else`/
  `#endif` and dead branches render in the tree, and will churn the
  `parser_diff` suite; it warrants its own plan. Until it lands, the two
  `SyntaxKind` variants are reachable only through `TriviaToken::directive_kind`
  (tested, but not yet spliced into a tree) — the same "data model landed,
  consumer deferred" shape as PR #157. **That plan now exists:** see
  `docs/completed/parser-ifdef-plan.md`.
- **CC-directive trivia kinds** `HASH_IF` / `HASH_ELSE` / `HASH_ELIF` /
  `HASH_ENDIF` (plus `INACTIVECODE`). FCS emits these too under `skip=false`;
  a fully lossless ifdef-aware tree needs them. They are folded into the
  green-tree consumer's first stage — `docs/completed/parser-ifdef-plan.md` Stage C1.
- **`#line` payload threading.** The `Directive::Line { number, file }` and
  `#nowarn`/`#warnon` `numbers` payloads already parse (PR #157) but are not
  carried on the emitted trivia token here (the token is payload-free, like
  FCS's `HASH_LINE`, which keeps the line number on the lexbuf, not the
  token). Threading payloads to a request handler overlaps the
  "Structured payloads" follow-up and the `LineDirectiveStore` work in
  `docs/completed/line-directive-remap-plan.md`; revisit with the consumer.

## Notes on existing consumers

`lex_with_symbols` and `Driver` are unchanged, so every current caller is
untouched:

- `crates/lsp/src/diagnostics.rs` (`diagnostics_for`, `line_directive_store`)
- `crates/cst/tests/corpus.rs`, `tests/lexer_diff.rs`,
  `tests/lexfilter_corpus.rs`
- the `driver.rs` unit/PBT suite

The `lexer_diff` / `parser_diff` / `lexfilter_diff` differential surfaces do
not observe the new mode (the first two filter trivia / use bare `lex`; none
call `lex_with_symbols_full_trivia`), so there is no FCS-dump churn.
