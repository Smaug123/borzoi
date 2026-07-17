# EOF-as-column-−1 for offside diagnostics plan

Status: **landed** (PR #900). Follow-up to Stage 5 (§A) of
[`offside-diagnostics-plan.md`](../offside-diagnostics-plan.md): makes the general
offside FS0058 — and its push-abort recovery — fire when the anchoring token is
the synthetic EOF, mirroring FCS's `ColumnMinusOne` EOF rule
(`startPosOfTokenTup`, LexFilter.fs:640). All three motivating cases
(`let f = function\n`, `match x with\n`, `try x with\n`) now emit FS0058 at
line 2 col 0 — error at 8.0+, warning at 7.0.

## What changed

FCS reads the EOF token as column −1 ("processed as if on column -1 … forces the
closure of all contexts"), so a context anchored at EOF is offside of a col-0
enclosing limit. Our port modelled EOF at its true byte column and so missed the
diagnostic. The fix threads an `anchor_is_eof` flag through the push path so only
the offside *comparison* sees −1; the stored `Pos` stays the true byte column
(used for the FS0058 message and virtual anchoring, so `utf16_col` never slices
with −1). Because the head-context force-closure cascade empties the stack before
any offside-*pop*, EOF never reaches a pop site — the −1 bites only at
lookahead/anchor pushes.

## Landed stages (one line each) — all in PR #900

- **Stage 1** — thread `anchor_is_eof` through `push_ctxt`/`try_push_ctxt` →
  `is_correct_indent` at the four anchor sites (FUNCTION, WITH+match/try,
  `push_ctxt_seq_block_at`, `peek_initial`), `false` elsewhere; no behaviour change.
- **Stage 2** — turn on the subtraction `c2 = ctxt.start_pos().col as i32 -
  i32::from(anchor_is_eof)`, so the already-anchored FUNCTION `CtxtMatchClauses`
  (`let f = function\n`) emits FS0058 at EOF; `limit ≥ 1` shapes (`let x =\n`,
  `module M =\n`) provably unchanged (regression-guarded).
- **Stage 3** — drop the `!Eof` guard on the WITH+match/try `CtxtMatchClauses`
  push (`with_dispatch`), so `match x with\n` / `try x with\n` push the
  EOF-lookahead context: at 8.0+ the strict push emits then aborts (no spurious
  OEND); at 7.0 it warns and keeps the context, closed by the EOF cascade's OEND.

Oracle: `assert_offside_spans_match_at_langversion` (the below-8.0 warning
counterpart of `assert_offside_spans_match`) added for the 7.0 cases; both corpus
gates green. Tests live in `crates/cst/tests/all/parser_diff_offside.rs`.
