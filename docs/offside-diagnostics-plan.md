# Offside / indentation diagnostics (FS0058) plan

> **Status:** Stage 1 (infra), Stage 2 (nested-construct §B–D), and Stage 5 (the
> general offside §A) are landed. Stages 3 (§H `|`-misalignment), 4 (§G
> `in`-misindentation), and 6 (RecordTypes-class parser recovery) remain — full
> detail under "Still to do". Everything above that section is a compact record
> plus the FCS reference table the §-labels in the code point at.

Two families of parser-vs-FCS divergence share one root cause: FCS's lex-filter
reports **indentation / offside** problems (all FS0058) that ours used to drop.
We were *too lenient* (accepting offside code FCS rejects — the bulk of the
`we_accept_fcs_rejects` corpus bucket) and *too strict* (rejecting valid offside
layouts FCS accepts — the `RecordTypes.fs` dedented-attribute shape, Stage 6).
FS0058 is a real error an agent editing F# wants surfaced, and matching FCS is
the project's standing goal.

## Landed stages (one line each)

- **Stage 1** (PR #870) — diagnostics channel + language-version threading:
  `OffsideDiagnostic` / `OffsideSeverity`, `filter_collect` returning a
  `FilterRun` (`{ filtered stream, diagnostics }`), `LanguageVersion` threaded
  into the filter and resolved to `strict_indentation_is_error` (F# 8+) and
  `reports_invalid_decls_in_type` (F# 10+); parser switched to `filter_collect`
  and merges diagnostics into `Parse.errors`/`warnings` (`parser/mod.rs:344`).
- **Stage 2** (PR #871) — §B/C/D nested `type`/`module`/`exception` in a type
  body: `check_invalid_decl_in_type_defn` (`lexfilter/pushes.rs:115`) ported from
  FCS `checkForInvalidDeclsInTypeDefn`, wired from the Type/Module/Exception
  arms, gated on `reports_invalid_decls_in_type` (F# 10+).
- **§E** (PR #462, pre-dates this plan) — nested `open` in a type body is emitted
  and recovered parser-side (`decls_type.rs`, `stray_open_in_type_body_span`); the
  lex-filter deliberately does *not* re-emit it. This is the template the
  remaining recovery work follows.
- **Stage 5 / §A** (PR #881; EOF-anchor follow-up PR #900) — general offside
  `TokenIsOffsideOfContextStartedEarlier` emitted from `Filter::is_correct_indent`
  (`lexfilter/mod.rs:1758`), a faithful port of FCS's `isCorrectIndent`. Severity
  from `strict_indentation_is_error` (warn <8.0 / error ≥8.0); span at the
  trigger token; message position printed in UTF-16 units (`utf16_col`).
  Dropped `we_accept_fcs_rejects` 44→30 with no `we_reject_fcs_accepts`
  regressions and no token-stream change.

## Reference: how FCS produces these (all FS0058)

Grounded in `../fsharp/src/Compiler/SyntaxTree/LexFilter.fs`,
`FSComp.txt`, and `CompilerDiagnostics.fs`. Every diagnostic funnels through
`reportDiagnostic` → `IndentationProblem` → FS0058 (`CompilerDiagnostics.fs:278`);
the distinct `lexflt*` identifiers are only message strings. FCS **recovers** in
every case, so the parse tree is still produced — only the diagnostic is added.
Code comments refer to these constructs by the letters in this table (`§A`,
`§B–F`, `§G`, `§H`).

| # | `lexflt*` string | Severity | Gate (feature → version) | Status |
|---|---|---|---|---|
| A | `TokenIsOffsideOfContextStartedEarlier` | warn (F#7-) / error (F#8+) | `StrictIndentation` → 8.0 | **landed** (Stage 5) |
| B | `InvalidNestedTypeDefinition` | error | `ErrorOnInvalidDeclsInTypeDefinitions` → 10.0 | **landed** (Stage 2) |
| C | `InvalidNestedModule` | error | 10.0 | **landed** (Stage 2) |
| D | `InvalidNestedExceptionDefinition` | error | 10.0 | **landed** (Stage 2) |
| E | `InvalidNestedOpenDeclaration` | error | 10.0 | **landed** (parser-side, PR #462) |
| F | `InvalidNestedConstruct` (fallback) | error | 10.0 (currently unreachable) | n/a |
| G | `IncorrentIndentationOfIn` | warning (always) | none | **Stage 4** |
| H | `SeparatorTokensOfPatternMatchMisaligned` | error (always) | none | **Stage 3** |

Our parse default is **F# 10.0** (`LanguageVersion::DEFAULT`, frozen); `tools/fcs-dump`
parses at the same default, so the differential oracle sees all of these as the
corpus does.

### Load-bearing finding (applies to every remaining stage)

**Emission alone never satisfies the differential oracle — FCS's *recovery* must
match too.** `assert_asts_match_with_diagnostic` (`tests/all/common/mod.rs:1411`)
requires the normalised trees to be *equal*, so an FS0058 on top of divergent
recovery still fails. Several cases carry a *secondary* FCS diagnostic
(e.g. nested type also gets FS0547; nested open also gets FS0010) that can perturb
tree recovery; where it does, use `assert_asts_match_allow_errors` (both error,
trees agree) plus a span check, or extend the helper to compare only the FS58
diagnostic — decide per-case. This is why §E and Stage 6 are per-construct parser
work, not lex-filter one-liners.

---

## Still to do

### Stage 3 — Pattern-match `|` misalignment (§H / D3-H)

**Dependencies**: Stage 1 (done). Orthogonal to the rest. Always an error, ungated.

FCS `SeparatorTokensOfPatternMatchMisaligned` (`LexFilter.fs:2099-2110`): when the
top of stack is `CtxtMatchClauses(leadingBar, offsidePos)` and the incoming `BAR`
is off by exactly one column vs the clause alignment (the two slack-differing
offside conditions disagree).

The `MatchClauses` pop arm already computes the shifted column comparison
(`lexfilter/offside_pops.rs:647`, the `shift` table for BAR/END with
`leading_bar`) and closes the context — but emits **no** diagnostic. Add the
error push there: compute the two slack-differing conditions and push an
`OffsideDiagnostic { severity: Error }` when they disagree.

**Correctness oracle**:
- `assert_asts_match_with_diagnostic(<one-column-misaligned match>, 58)`.
- Property: over generated `match` clause lists, our error fires iff the `|` is
  off by exactly one column vs the clause alignment (matches FCS).

### Stage 4 — `in` misindentation (§G / D3-G) + the warning oracle

**Dependencies**: Stage 1 (done). Orthogonal. Always a warning, ungated.

FCS `IncorrentIndentationOfIn` (`LexFilter.fs:1679-1685`): top of stack is
`CtxtLetDecl(_, offsidePos)`, incoming `IN`, `inTokenCol < offsidePos.Column`.

The `in`-balances-`CtxtLetDecl` arm is present but the warning is explicitly
elided — see the marker at `lexfilter/offside_pops.rs:310-312` ("diagnostics
aren't wired yet; first input that misindents `in` will force that port"). Replace
the marker with a warning push. This is the smallest emission (one site, ungated)
and forces the **warning-oracle infrastructure** Stage 3's error oracle does not
need but which is worth having.

**Correctness oracle**:
- Add `assert_asts_match_with_warning(src, 58)` — FCS `ParseHadErrors == false`,
  ≥1 FS58 *warning*, trees agree, our warning span matches FCS's. (This helper
  does **not** exist yet — `tests/all/common/mod.rs` has only the error-comparing
  `assert_asts_match_with_diagnostic`; fcs-dump already exposes `Severity` and the
  diagnostic-span extractor.) Test: `assert_asts_match_with_warning(<let with
  dedented `in`>, 58)`.

### Stage 6 — RecordTypes-class parser recovery (D4)

**Dependencies**: Stage 2 (done — so genuinely-offside nested decls now error
instead of being silently accepted). Ideally also Stage 5 (done). This is a
**parser** change (`parser/decls.rs`), not a lex-filter one.

The motivating too-strict case: `FSharp.Core.UnitTests`' `RecordTypes.fs`, a
`type R` whose body dedents to column 1 for a leading `[<Struct>]` attribute, then
a column-0 `[<StructLayout>]` + `type T`. A heuristic fix in isolation was
abandoned after three `codex review` rounds because a blunt recovery that accepts
the dedented attribute *also* accepts genuinely-offside declarations
(`type`/`open` at a nested column), which FCS rejects with FS0058.

Now that Stage 2/§E emit FS0058 for those offside declarations, the module-decl
loop recovery becomes safe. The abandoned heuristic (clear `needs_sep` when a
consumed `ODECLEND` precedes the cursor) is correct **when scoped to a real
offside dedent**: a newline separates the previous real token from the cursor
(`while true do () done 42` is same-line ⇒ excluded) and the body has dedented to
`depth == 0` (a member `ODECLEND` inside an open type body is excluded). So:
- `type R … [<Struct>] (col1) … type T (col0)` → recover, accept, no FS0058
  (col-0 decl is at module offside) → matches FCS. ✓
- `… [<Struct>] (col1) … type T (col1)` → recover the tree, **but Stage 2 emits
  FS0058 nested-type** → we reject with the same error as FCS. ✓

**Correctness oracle**:
- `assert_asts_match("type R =\n    member _.X = 1\n\n [<Struct>]\n[<StructLayout>]\ntype T =\n    { Z : int }\n")`
  (the RecordTypes shape) — clean parse, tree matches FCS.
- Regression guards: the depth guard (`… member … open System`) and newline guard
  (`while true do () done 42`) still error, and the col1-attr+col1-type case emits
  FS58 (via Stage 2) matching FCS.
- Corpus: `RecordTypes.fs` and siblings move from `we_reject_fcs_accepts` to
  match; raise `MIN_AST_MATCHES` (currently 5452 in `parser_corpus_diff.rs`).
  As diagnostics land, also re-measure and tighten `MAX_WE_ACCEPT_FCS_REJECTS`
  (currently 31); investigate any file where we now emit an FS58 FCS does not (an
  offside-arithmetic edge).

## Non-goals / risks

- **No `--strict-indentation` CLI override** (we have no compiler CLI); severity
  is purely language-version-derived.
- **Secondary non-offside diagnostics** (FS0547, FS0010 companions) are not in
  scope; assert the FS58 diagnostic and tolerate FCS extras, adjusting the oracle
  where the extra changes tree recovery (see the load-bearing finding above).

Each remaining stage lands on its own branch, stacked as necessary, so a reviewer
can review each in isolation.
