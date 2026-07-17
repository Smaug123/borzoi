# Conditional-compilation-aware parsing (the green-tree consumer)

> **Status: completed.** All stages — C1a / C1b / C2 / C3 / C4 / C5 — landed
> (each marked *(done)* inline below). This was the "green-tree consumer" left
> deferred by `docs/completed/hashline-warndirective-trivia-plan.md` (*Out of scope*):
> making `cst::parser::parse()` conditional-compilation aware so the rowan
> green tree reflects only the **active** compilation branch, is **lossless**
> (`text(tree) == source`), and carries the directive / inactive-code lines
> as trivia. The trivia infrastructure it builds on (`SyntaxKind::HASH_LINE`
> / `WARN_DIRECTIVE`, `TriviaToken`, the full-trivia driver mode) landed in
> PRs #246 / #247 / #250.

Implement this plan with each stage on its own branch, stacked as necessary
on previous branches, so that a reviewer can review each branch in
isolation.

## Goal

Today there are two independent pipelines:

- **The green tree** (`cst::parser::parse`, `crates/cst/src/parser/mod.rs:57`)
  is built from bare `lex()` → `filter()` → recursive descent. It has **no
  preprocessor handling**: it parses *every* `#if` branch and the directive
  lines themselves, so dead branches produce spurious tokens / `ERROR`
  nodes and parse diagnostics (`crates/lsp/src/diagnostics.rs:20-26`).
- **The directive driver** (`lex_with_symbols` / `lex_with_symbols_full_trivia`,
  `crates/cst/src/directives/driver.rs`) is conditional-compilation aware
  but feeds only the LSP's lexer diagnostics — never the tree.

This plan unifies them: `parse()` consumes the directive driver, so the
tree contains only active-branch code (correct AST, no spurious dead-branch
errors) while remaining lossless. This is what an editor needs before
hover / semantic-tokens / format can respect `#if`.

## What FCS does (the target shape)

FCS layers the preprocessor *below* the lexfilter: `lex.fsl` skips inactive
branches, and under `skip=false` (editor mode) emits, for every source byte,
exactly one of:

- a real token (active branches),
- a directive trivia token — `HASH_IF` / `HASH_ELSE` / `HASH_ELIF` /
  `HASH_ENDIF` (`lex.fsl:1010-1063`), `HASH_LINE` (`lex.fsl:757-811`),
  `WARN_DIRECTIVE` (`lex.fsl:1084-1089`),
- an `INACTIVECODE` token spanning a dead `#if`/`#else` region
  (the `ifdefSkip` state, `lex.fsl:1101-1223`).

All of these are **hidden tokens**: declared in `pars.fsy:154-155` but
consumed by no grammar rule, so the **parsed AST contains only active-branch
nodes** — dead branches and directive lines never appear as `SynModuleDecl`
/ `SynExpr`. The lexfilter treats them as trivia for offside purposes. Our
design mirrors this exactly: the directive layer sits below `filter()`, and
the new tokens are rowan trivia kinds elided by the differential normaliser.

## Design

### Pipeline

Current (`parse`):

```text
lex(source) ──► raw_tokens ─┐
                            ├─► Parser walks raw+filtered in lockstep ─► tree
filter(raw_tokens) ─► filtered_tokens ─┘
```

Proposed (`parse_with_symbols`):

```text
lex_with_symbols_full_trivia(source, symbols)  ─►  full: Vec<CoreItem>   (byte-complete)
        │                                              │
        │  (the lossless "raw" walk; every byte covered by some token)   │
        ▼                                              ▼
   active substream  =  full.filter(Lexed → Token; active Lex error → LexError)
        ▼
   filter(active)  ─►  filtered_tokens
        ▼
   Parser walks raw = full, filtered = filtered  ─►  tree
```

Two facts make this clean:

- **`filter()` already takes an arbitrary `Token` iterator**
  (`crates/cst/src/lexfilter/mod.rs:582`), and offside columns come from the
  active tokens' real source positions — feeding it the active substream is
  exactly FCS's preprocessor-below-lexfilter layering.
- For a **directive-free** source, the full-trivia active substream equals
  `lex(source)` token-for-token (guaranteed by the existing driver PBTs:
  `fast_matches_reference_on_balanced_sources` + B2's additive-equivalence).
  So `parse(source) := parse_with_symbols(source, &HashSet::new())` is a
  no-op for every existing (directive-free) `parser_diff` fixture.

### The "raw" stream becomes byte-complete `TriviaToken`s

The parser's lossless guarantee (`text(tree) == source`) holds because it
walks a raw stream covering *every* byte (`emit_text` +
`raw_consumed_end`, `parser/mod.rs:137-178`). For that to survive the
directive layer, the full-trivia stream must **partition `[0, source.len())`
with no gaps or overlaps**: active tokens over active bytes, directive
trivia over directive lines, `INACTIVECODE` over dead regions. B2's mode is
*not* yet byte-complete — it drops dead regions and the CC-directive lines —
so Stage C1 extends it.

### New token kinds (subsumes the deferred CC-directive trivia)

`SyntaxKind` + `TriviaToken` gain: `HASH_IF`, `HASH_ELSE`, `HASH_ELIF`,
`HASH_ENDIF`, `INACTIVECODE`. All are trivia (`SyntaxKind::is_trivia`), so
the differential normaliser elides them (`tests/common/normalised_ast.rs`
walks structural `children()` only) and `parser_diff` is unaffected by their
presence. This is the CC-directive-trivia item deferred in
`docs/ifdef-plan.md` and `docs/completed/hashline-warndirective-trivia-plan.md`.

### `parse` signature & error reconciliation

- Add `pub fn parse_with_symbols(source: &str, symbols: &HashSet<String>) -> Parse`;
  keep `parse(source) = parse_with_symbols(source, &HashSet::new())` so no
  existing caller breaks.
- The driver yields `PreprocError`; the parser's raw walk currently keys on
  `LexError`. Reconciliation rule: `PreprocError::Lex(e)` in an **active**
  branch → an `ERROR` token + `ParseError` (today's behaviour for live lex
  errors). Structural directive errors (`UnmatchedEndIf`, `OrphanElse`, …)
  are **not** re-surfaced as parse errors — they are already the LSP lexer
  producer's job (`diagnostics_for`); for the tree the malformed directive
  line is emitted as its trivia kind (lossless), nothing more. This keeps
  the parser from double-reporting what `diagnostics_for` owns.

### Key risks (the lossless proptest is the oracle for all three)

1. **Newline / boundary assignment.** Which token owns the `\n` after a
   `#if` line, and exactly where `INACTIVECODE` starts/ends, decides whether
   the partition is gapless. Pin with the byte-completeness PBT (C1).
2. **Lexfilter virtuals across dead-region gaps.** A virtual inserted at the
   end of an active region anchors to the *next* active token's span, which
   may sit across an inactive gap; `bump_into` drains raw up to that point
   (`parser/mod.rs:215-223`), so the intervening directive/inactive trivia
   flush correctly — but this is the subtlest interaction. The lossless
   proptest over directive-bearing sources (C2) is the guard.
3. **Offside correctness across `#if`.** Mitigated by construction: feeding
   the active substream to `filter()` is FCS's own layering, so offside
   columns are computed from the same tokens FCS sees.

## Implementation plan

Stage C1 lands in two halves, mirroring stages A / B2: **C1a** the
vocabulary (here, alongside this doc), **C1b** the emission.

### Stage C1a — `SyntaxKind` + `TriviaToken` vocabulary (done, lands with this doc)

**Dependencies**: none (builds on the merged B2 driver).

**Implements**: the FCS `HASH_IF` / `HASH_ELSE` / `HASH_ELIF` / `HASH_ENDIF`
/ `INACTIVECODE` token kinds. Adds the five `SyntaxKind` variants (before
`__LAST`, classified trivia in `is_trivia`) and the five `TriviaToken`
variants; generalises the driver→tree bridge
`TriviaToken::directive_kind` → `trivia_syntax_kind` to map every trivia
marker (the rename is accurate now that `INACTIVECODE` — not a directive — is
covered). No emission yet: the new markers are constructed by no code, the
same "vocabulary ahead of producer" shape as stage A.

**Correctness oracle**:

- `from_raw` round-trips every discriminant (the existing sweep covers the
  five new ones); each new kind is `is_trivia`.
- `trivia_syntax_kind` maps each new `TriviaToken` marker to its kind
  (unit test); the existing additive-equivalence / span-correspondence /
  totality PBTs stay green (they drop *all* non-`Lexed` markers, so they are
  robust to the new variants).

### Stage C1b — Byte-complete emission (done)

**Dependencies**: C1a.

**Implements**: the full-trivia mode now emits a CC-directive trivia token
(`HASH_IF`/`HASH_ELSE`/`HASH_ELIF`/`HASH_ENDIF`) over each *visible*
directive line and one `INACTIVECODE` token over each dead `#if`-eliminated
region, so the emitted spans tile the source. A gap-fill cursor
(`covered_end`) coalesces a dead region — including any non-visible nested
directives inside it — into one `INACTIVECODE` span; visible directives
(parent-active) are emitted, nested ones absorbed.

**Correctness oracle**:

- **Byte-completeness PBT** (`full_trivia_tokens_partition_source`): the
  emitted `Ok` spans satisfy `first.start == 0`,
  `span[i].end == span[i+1].start`, `last.end == source.len()`, and
  `concat(source[span_i]) == source`. (On the balanced generator, which has
  no `Err` items.)
- **`INACTIVECODE`-over-dead PBT** (`inactive_code_covers_only_dead_bytes`):
  every `INACTIVECODE` byte is inactive per the reference `active_mask`.
- **Additive-equivalence still holds**: dropping *all* markers and
  unwrapping `Lexed` recovers `lex_with_symbols` exactly (so the active
  token + error stream is provably unchanged).
- **Span-correspondence**: the reference walk now also enumerates visible CC
  directives; the stream's directive markers (excluding `INACTIVECODE`)
  equal it.
- Totality; example tests pinning the `#if/#else/#endif` partition, a nested
  dead region collapsing into one `INACTIVECODE` span, and a malformed
  (unterminated `(*`) dead branch that is covered by `INACTIVECODE` and never
  lexed. (Malformed-dead is covered by example tests rather than a generator
  extension — putting malformed bytes in a generated *active* branch would
  produce lex errors and a spurious partition gap.)

### Stage C2 — `parse_with_symbols`: the parser consumes the driver (done)

**Dependencies**: C1.

**Implements**: `parse_with_symbols`; the pipeline rewire (raw = byte-complete
full-trivia stream; active substream → `filter`); the parser raw-walk over
the new `RawTok = (Result<TriviaToken, PreprocError>, Range)` — `drain_raw_up_to`
emits `Lexed` trivia / directive / inactive-code markers at their kind. The
~20 raw-stream lookahead helpers unwrap `Lexed` via `raw_is_trivia` /
`raw_significant` / `raw_trivia_kind`. **Structural directive errors**
(`UnmatchedEndIf`, `OrphanElse`, …) are *filtered out of the raw stream at
construction* (the reconciliation rule: their bytes are already covered by
the directive's trivia token, and the LSP lexer producer owns the
diagnostic). Filtering at the source — rather than only skipping them in the
drain — is what keeps them from acting as phantom `Err` stoppers in the
lookahead helpers (a review caught the latter). Only `PreprocError::Lex`
(a genuine active lex failure) remains as an `Err`, and it correctly stops a
scan / becomes an ERROR node. `parse(source)` is `parse_with_symbols(source,
&∅)` — a single path, no leftover bare-`lex` pipeline.

**Correctness oracle** (all green, `crates/cst/tests/parser_ifdef.rs`):

- **Lossless proptest** — `text(parse_with_symbols(s, syms).tree) == s` for
  arbitrary directive-bearing sources. This invariant had no test before;
  it is the primary guard and exercises the lexfilter-virtual-across-gaps
  interaction the design flagged as the main risk.
- **`parser_diff` stays green** (497 fixtures): every directive-free fixture
  is byte-for-byte unchanged, since the active substream equals `lex(source)`
  there (a property already pinned by the driver PBTs).
- **No spurious dead-branch errors**: a malformed `(*` in a dead branch
  produces zero parse errors (it is `INACTIVECODE`, never lexed).
- **Active-branch selection**: `normalise_parse` of a directive-bearing
  source equals that of the selected branch parsed alone, in both the
  symbol-defined (`then`) and undefined (`#else`) directions.

**LSP ripple (pulled forward from C5):** because `parse` is now ifdef-aware,
the LSP's `parse_diagnostics` no longer squiggles directive lines / dead
branches. A review caught that keeping `parse_diagnostics` on the empty
symbol set would be a *correctness regression* — a syntax error in a live
`#if FOO` branch (FOO defined by the project) would read as dead and go
unreported — so `parse_diagnostics` now calls `parse_with_symbols(text,
symbols)`, the same set the lexer producer uses. Both producers thus agree on
the active branches; regression guards pin that an active-branch error *is*
reported and an inactive-branch one is *not*. Only the now-vestigial
overlap-dedup cleanup remains for C5.

### Stage C3 — `parser_diff` directive fixtures (undefined symbols) (done)

**Dependencies**: C2.

**Implements**: seven `diff_ast_*` fixtures in `crates/cst/tests/parser_diff.rs`
exercising `#if`/`#else` (else selected), no-`#else` (then dead), an `#elif`
chain falling to `#else`, a nested `#if` inside a dead branch, a directive
splitting a `let` RHS (offside across the inactive gap), and the `#nowarn` /
`#line` trivia directives — all using *undefined* symbols so the active
branch is `#else` / post-`#endif`.

**Correctness oracle**: our normalised AST equals `fcs-dump ast`, confirmed
green against the **unmodified** dump tool. `dumpAst` parses with empty
defines (`tools/fcs-dump/Program.fs:118-121`,
`GetParsingOptionsFromCommandLineArgs` with no `--define`), so FCS drops the
`#if <undefined>` branch exactly as our empty-symbol-set parse does — the two
agree by construction (probed empirically before relying on it). This pins
that C2's branch selection + directive-trivia handling matches real FCS, not
just our own normaliser.

### Stage C4 — Defined-symbol fixtures (done)

**Dependencies**: C3.

**Implements**: threads defines through `tools/fcs-dump` — `dumpAst` takes a
symbol list and sets `FSharpParsingOptions.ConditionalDefines` (the field is
`ConditionalDefines`, not `ConditionalCompilationDefines`); the CLI accepts
`ast <file> SYM…`. The harness gains `invoke_fcs_dump_with_defines` and
`assert_asts_match_with_defines`, which pass the same symbol set to both
`fcs-dump` and `parse_with_symbols`. Three defined-symbol `diff_ast_*`
fixtures where the *then* branch is active: `#if/#else` then-selected,
no-`#else` then active, and a nested `#if` with both symbols defined.

**Correctness oracle**: each matches `fcs-dump ast <file> SYM…` (probed
manually first: no-define → `#else`, `FOO` → then-branch). `parser_diff`:
504 → 507.

**`#elif` caveat:** no defined-symbol `#elif` fixture. `fcs-dump`'s default
editing language version does not enable `LanguageFeature.PreprocessorElif`,
so FCS rejects `#elif` ("Unexpected keyword 'elif' in directive") and treats
every `#elif` arm as inactive (`#if FOO`(false) `/ #elif BAR`(defined) selects
*neither*). Our parser implements modern-F# `#elif`, so a defined-symbol elif
fixture would diverge on the dump config, not on parser correctness; enabling
the feature would mean threading a language version through every `ast` dump
and re-validating all non-directive fixtures — disproportionate here. The
shared falls-through-to-`#else` behaviour is pinned by C3's
`diff_ast_ifdef_elif_chain_falls_to_else`.

### Stage C5 — LSP: correct the overlap-dedup documentation (done)

**Dependencies**: C2.

The `parse_diagnostics` → `parse_with_symbols(text, symbols)` switch already
landed in C2. This stage set out to *retire* the overlap-dedup, but a probe
showed that premise was wrong: for an active-branch lex error (`let x = "oops`)
the parser emits a spurious structural **cascade** (two errors) at the
lex-error span, and the dedup is exactly what keeps that from squiggling three
times. So the dedup is **kept** — it is not vestigial — and the stage instead
corrects the stale documentation around it.

**Implements**:

- The module note and `parse_diagnostics` doc no longer claim the parser "has
  no preprocessor handling" / "re-derives the same failures" / that
  "inactive-branch errors survive"; they now describe the dedup's real job
  (suppressing the active-branch lex-error cascade) and note that structural
  directive errors are filtered out of the parser's raw stream entirely.
- The directive case *is* obsolete: the parser reports nothing for an orphan
  `#endif`, so `parse_diagnostics_dedups_directive_errors` is renamed to
  `parse_diagnostics_does_not_report_directive_errors` and strengthened to
  assert the parser produces zero diagnostics there (the dedup is vacuous for
  directives, real only for the lex cascade).

**Correctness oracle**:

- The LSP diagnostics tests stay green, including the strengthened directive
  test and the two dedup proptests (no surviving parser diagnostic overlaps a
  lexer diagnostic, over arbitrary + directive-shaped input).
- The C2 regression guards (active-branch error reported, inactive-branch
  error not) stay green.

## Out of scope

- **Editor features this unblocks but does not deliver**: semantic-tokens
  (dimming `INACTIVECODE`, colouring directive lines), hover, and format —
  each its own effort consuming the now-ifdef-aware tree.
- **`#line` span remapping** into virtual coordinates —
  `docs/completed/line-directive-remap-plan.md`.
- **Module-level `SynModuleDecl.HashDirective`** (`#load` / `#r` / script
  directives parsed as *AST nodes*, not trivia) — `docs/parser-plan.md`
  phase 10; orthogonal to the trivia-token approach here.

## Notes on ordering

C1 → C2 is the critical path. C3 and C5 are independent after C2 (one
strengthens the oracle, one realises the diagnostic benefit); C4 stacks on
C3. The lossless proptest added in C2 is the load-bearing oracle — land it
early and keep it green through C3–C5.
