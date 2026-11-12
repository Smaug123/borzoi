# Token pipeline as struct-of-arrays passes

> **Status (2026-11-12).** Stage 0 (count harness), Stage 1 (the raw token
> table), Stage 2 (the frozen filter reference), Stage 3 (the filtered
> token table), Stage 4 (the filter reads the raw table by index) and
> Stage 5 (the newline table) implemented.
> Tracks [#542](https://github.com/WoofWare/borzoi/issues/542). Stacked on the
> flat syntax table (`docs/flat-syntax-tree-plan.md`), which changed the
> parser's tree builder.

## Problem

A parse runs three passes before the productions see a token, and each hands
the next a vector of fat enums.

1. **Lex.** `directives::lex_with_symbols_full_trivia` wraps
   `lexer::interp::InterpDriver`, which wraps logos. Every token comes out as
   `(Result<TriviaToken<'a>, PreprocError>, Range<usize>)`, about 56 bytes, and
   in full-trivia mode passes through a `VecDeque` on its way. `parse_inner`
   collects them into `Vec<RawTok>`.
2. **Lex-filter.** `lexfilter::filter_collect` consumes a *clone* of the
   active tokens, turns each into a `TokenTup` (token, `Range<usize>` span,
   two `Pos`), scans every byte of every token and gap again for `\n` to keep
   its line cursor (`Filter::advance_cursor_to`), moves `TokenTup`s through
   its `Step` dispatch and its delayed stack, boxes a `Token` for every
   `ODUMMY` it queues, and collects `Vec<(Result<FilteredToken, LexError>,
   Range<usize>)>`.
3. **Scans.** `sign_fold` rewrites that vector, and three diagnostic scans
   (`tab_diagnostics`, `reserved_ident_diagnostics`,
   `operator_char_diagnostics`) each walk the whole raw vector again.

Then the productions walk both vectors in lockstep (`Parser::raw_tokens`,
`Parser::filtered_tokens`).

Measured with Stage 0's harness (`crates/sema/examples/parse_counts.rs`) on
the stack tip (5295c60c), over the corpus's `src/` tree (259 files, 11.8 MB,
2.73 M raw tokens, 0.232 per byte), min of 3. Instruction and allocation
counts are the measure; the machines are loaded, so wall time is printed for
orientation only.

| phase | instructions / byte | allocations / byte | bytes allocated / byte | wall (loaded) |
|---|---|---|---|---|
| lex (full trivia, collected) | 75.7 | 0.0004 | 43.2 | 5.2 ns/B |
| lex-filter (from collected raw) | 242.9 | 0.0272 | 23.4 | 16.0 ns/B |
| parse (everything) | 546.3 | 0.0356 | 109.2 | 35.4 ns/B |

So lexing plus filtering is 319 of a parse's 546 instructions per byte, and
the productions, scans and table build the remaining ~228. A profile of
`parse` on the same tree puts `filter_collect` at 40%,
`Filter::hw_token_fetch` at 35% inclusive and 13.6% self, the productions at
29.5%, lexing (`DriverCore`) at 13%, `TreeBuilder::finish` at 7.3%, `memmove`
at 4.5% (mostly `TokenTup`s moved between filter steps), and malloc/free at
3%. The lexer's 43 bytes allocated per byte are the growth of a vector of
56-byte items; the filter's 0.027 allocations per byte are mostly `ODUMMY`
boxes.

The issue's target is 10–20 ns per byte for the parse. Raw logos alone runs
at about 13.7 instructions per byte, so most of today's lex and filter cost is
representation and plumbing, not the work. The productions' share is not
addressed here beyond what a smaller token representation buys them.

## Design

### D1. Two token tables, each a struct of arrays

**The raw table** holds the lossless full-trivia stream: every token, trivia,
directive line and inactive region, in source order. It is
`RawTokens { kinds: Vec<RawKind>, starts: Vec<u32> }` with `starts` one longer
than `kinds`: the stream tiles the source (the full-trivia driver already
guarantees that, `DriverCore::fill_inactive_gap`), so token `i` spans
`starts[i]..starts[i + 1]` and the last entry is the source length. That is 6
bytes per token, against 56.

**The filtered table** holds the stream the productions read: kinds, starts
and ends (`u16`, `u32`, `u32`), 10 bytes per token. Its spans are not a tiling
(virtual tokens are zero-width or synthetic, `RQuoteBarRBrace` splits into two
overlapping tokens, sign-folding merges two), so it keeps both ends.

Offsets are `u32`. The syntax table already refuses a source of 4 GiB or more
(`syntax::flat`, "a source is shorter than 4 GiB"); the token tables assert
the same precondition at the same place in the pipeline, the start.

### D2. Kinds are payload-free, and a token's payload is its text

Every `&'a str` payload of `lexer::Token` is the token's own text (logos's
default for a `&str` field is the matched slice, and no callback narrows it),
so `(kind, text)` determines the token. `Less(bool)` / `Greater(bool)` are
`false` at lex time; the filter's typar promotion is a filtered kind of its
own. `InterpString(InterpKind)` becomes one kind per `InterpKind` variant and
byte suffix; the extended forms' `n` is the count of leading `$`, which their
regex fixes.

- `TokenKind` (`#[repr(u16)]`): one variant per `Token` shape.
- `RawKind`: `Lexed(TokenKind)`, the directive and inactive-code trivia, and
  the three lexer errors. One dataful variant, so it niche-packs into 2 bytes
  (asserted at compile time).
- `FilteredKind`: a non-trivia `TokenKind`, a `Virtual`, a typar-promoted
  `<`/`>`, or a lexer error.

`Token::kind` and `Token::from_kind` are generated by one macro from one
variant list, so a `Token` variant missing from the list is a non-exhaustive
match, not a silent gap. Reconstructing a `Token` from `(kind, text)` is a
`match` and a slice, which is what lets the productions keep matching on
`Token` values while the storage changes underneath (D5).

### D3. Line numbers come from a newline table, not from rescanning tokens

The filter needs a `Pos { line, col }` for every token it pulls and the UTF-16
column of a context's anchor for FS0058 messages. Today it scans each token's
bytes and each gap's for `\n` as it pulls, and keeps a sparse `(line, start)`
list for the UTF-16 conversion.

The issue suggests taking lines from the lexer's own newline tokens. That is
not the right source: a lone `\r` is a `Newline` token but not a line break
for FCS (`newline = '\n' | '\r' '\n'`, `lex.fsl:315`), block comments,
strings and inactive regions hold line breaks no `Newline` token marks, and
the filter's input has directive lines and inactive regions removed, whose
lines FCS still counts. The exact definition is "the byte after every `\n` in
the source", so the table is that: `line_starts: Vec<u32>`, computed in one
pass over the bytes. A token's line is then found by a cursor that only moves
forward (the filter pulls in source order), and a line's start is an index,
not a search.

### D4. The filter runs on indices

The filter reads the raw table directly: it skips trivia by kind, and its
`TokenTup` becomes (filtered kind, `u32` span, two `Pos`), about 20 bytes and
`Copy`, with no `Token` payload. `ODUMMY`'s inner token becomes a `TokenKind`,
so the box goes. It writes the filtered table.

### D5. The productions keep their `Token` patterns

The parser has about 2,750 `Token::…` patterns. Rewriting them to compare
kinds would be the largest diff in the plan and buys little: matching a
reconstructed enum costs what matching a stored one does. So `Parser::peek`
and the raw-side accessors return a `Token` (or the old tuple) *by value*,
rebuilt from the tables, and the productions' patterns stay. A later stage
may move hot sites to kind comparisons where a profile says so.

### D6. Diagnostic scans fold only where measurement says so

The issue suggests folding the three diagnostic scans into the parser's
drain. Over a 2-byte kind array they become cheap scans in their own right,
and keeping each a pure function of the token table keeps it testable alone,
which a fold into the drain (which restarts with the parse, and stops early
at the depth limit) would not. So D1 comes first, and Stage 7 measures what
the scans cost over the table before folding any of them, into the lexing
pass rather than the drain.

### D7. The reference implementations

The pipeline is rewritten in place, so every stage is held to the code it
replaces, kept as a test-only reference behind a `token-reference` feature
of `borzoi-cst`, which this crate's own tests turn on (the
`rowan-reference` pattern):

- **The raw stream.** The full-trivia driver iterator stays the production
  lexer until Stage 6 replaces it with a table-writing pass, and the LSP
  keeps using the swallow-mode iterator. So it is the reference for the raw
  table throughout: the table must reconstruct exactly the stream the
  iterator yields, item for item, spans included.
- **The filtered stream.** Stage 2 freezes a verbatim copy of `lexfilter/`
  and `sign_fold` as the reference before anything in them changes. The
  filtered table must reconstruct exactly the stream the frozen copy
  produces from the iterator, with the same offside diagnostics and the same
  `shape_depends_on_language_version`.

Under the feature, `parse_inner` runs the reference beside the new pipeline
and asserts equality on every parse, so every test that parses holds the
stage to its oracle (the flat table's Stage 2 did the same with rowan), and
the cst corpus sweeps hold it over the whole corpus. Each stage also adds a
direct property over generated inputs: every one- and two-piece source over
the panic sweep's `WIDE` alphabet and over a lexical alphabet (every string,
interpolated-string, comment, numeric and directive shape, both line-break
spellings, a BOM, tabs, non-ASCII text and an unlexable byte), under symbol
sets that make the `#if` arms both active and inactive, plus a proptest
random walk over both.

The FCS lex-filter differential and the parser corpus manifests are
unchanged throughout; any movement in them is a defect in the stage.

## Implementation plan

> Implement this plan with each stage on its own branch, stacked as necessary
> on previous branches, so that a reviewer can review each branch in isolation.

Every stage lands alone with the full gate green and every manifest
unchanged, and reports its `parse_counts` before and after.

### Stage 0: count harness

**Dependencies**: none. **Implements**: the measurement every later stage
reports.

`crates/sema/examples/parse_counts.rs`: lex, lex-filter and parse over
`$BORZOI_CORPUS/src`, each fed the previous phase's output, reporting
instructions (macOS `proc_pid_rusage`), allocations and bytes allocated per
source byte, min of N, beside wall time. It lives in `borzoi-sema` because a
`borzoi-cst` example sees that crate's dev-dependencies, which turn on
`rowan-reference`. Each later stage updates its phases to the pipeline's
current entry points.

**Oracle**: it runs to completion under `nix develop` and reproduces the
baseline table above.

### Stage 1: the raw token table

**Dependencies**: Stage 0. **Implements**: D1 (raw table), D2 (`TokenKind`,
`RawKind`), D7 (raw stream reference).

`TokenKind`, `RawKind`, `Token::kind` / `Token::from_kind`, and
`tokens::RawTokens`, built in one pass from the full-trivia driver, with the
preprocessor errors and `#elif` spans the parse needs collected in the same
pass. The parser's raw side (`Parser::raw_tokens`), `sign_fold`, the three
diagnostic scans and the filter's input read the table; the raw-side
accessors return the old tuple by value (D5).

**Oracle**:
- The table reconstructs the driver iterator's stream exactly, over the
  generated inputs of D7 and, through the lockstep assertion in
  `parse_inner`, every parse in the cst suite and corpus sweeps.
- The tiling precondition holds on every input (asserted in the builder; the
  generated inputs reach the error, directive and inactive-code paths).
- `size_of::<RawKind>() == 2` at compile time.
- The kind-level trivia classification the hot paths use
  (`RawKind::trivia_syntax_kind`) agrees with the parser's classification of
  the rebuilt entry, on every kind (`parser/tests/token_kinds.rs`).
- `parse_counts`: lex allocates fewer bytes per byte.

**Result.** The table is `tokens::RawTokens`, built by
`tokens::lex_full_trivia`. The parser's raw-side scans rebuild the old tuple
only where a production reads it; the per-token paths (the drain, the bump,
the significant-token lookaheads, the position searches) read kinds and
offsets. Over the corpus's `src/` tree, min of 3, on the loaded machine:

| phase | instr/B before | instr/B after | alloc-B/B before | alloc-B/B after |
|---|---|---|---|---|
| lex | 75.7 | 83.6 | 43.2 | 4.1 |
| lex-filter | 242.9 | 257.9 | 23.4 | 23.4 |
| parse | 546.3 | 581.7 | 109.2 | 70.1 |

Bytes allocated fall by 36% over the whole parse, and the lexer's by 90%.
Instructions rise by 6%, in three places this stage leaves for later ones:
lexing converts each `Token` to a kind and back (+8, gone when the lexer
writes kinds, Stage 6); the filter's input rebuilds each active token from
the table (+15, gone when the filter reads the table, Stage 4; the filter
itself is unchanged, at 245 from a pre-built vector); and the productions'
remaining raw-side scans rebuild tuples (+12). Wall time, for orientation
only, fell from about 35 to 29 ns per byte.

### Stage 2: freeze the filter reference

**Dependencies**: Stage 1. **Implements**: D7 (filtered stream reference).

A verbatim copy of `lexfilter/` and `sign_fold` under
`token-reference`, fed by the driver iterator, plus the lockstep assertion
and the generated-input property comparing it with the live filter. At this
stage the two are the same code, so the property passes by construction; what
the stage proves is that the harness compares what it claims to (a planted
change in the live filter makes it fail).

**Oracle**: `diff -r` against the live module shows only module paths; the
property fails against a planted mutation and passes without it.

**Result.** `crate::reference` (under `token-reference`) holds the copy:
`reference/lexfilter/` is `lexfilter/` without its two unit-test modules and
`typars_close_op`'s inline tests, and `reference/sign_fold.rs` is the
pre-Stage-1 `parser/sign_fold.rs` with its imports replaced and its tests
dropped. `parser::parse_inner` compares each parse's filtered stream, offside
diagnostics and language-version flag with `reference::filtered_stream`'s,
using the two copies' identical derived `Debug` as the currency. Planting a
change in the live filter (dropping `)` from `is_atomic_expr_end`) fails the
panic sweep's `CORE` triples at `)-1` but no pair, lexical or `WIDE`, so the
triples are part of this oracle.

### Stage 3: the filtered token table

**Dependencies**: Stage 2. **Implements**: D1 (filtered table), D2
(`FilteredKind`), D5.

`filter_collect` writes a `FilteredTokens` table; `sign_fold` rewrites the
table; `Parser::peek` and its neighbours rebuild the old tuple by value. The
streaming `lexfilter::filter` stays, as a view over the table, for the FCS
differential and the LSP.

**Oracle**: lockstep and generated-input equality with the frozen reference
(tokens, spans, diagnostics, the language-version flag); FCS lex-filter
differential and manifests unchanged.

**Result.** The table is `tokens::FilteredTokens`, one `FilteredKind` and a
`u32` start and end per token. `FilteredKind` has two dataful variants
(`Token(TokenKind)` and `Virtual(Virtual)`), which do not share a niche, so it
is 4 bytes and a token 12, not 10; the lexer errors are unit variants of their
own, as in `RawKind`. `FilteredTokens::push` asserts that the filter emits no
trivia and, in a debug build, that each token's payload is its source text, so
the table can rebuild it. Only a lexed token's span is a slice of the
source: a virtual token placed after a line's last token takes its span from
that token's end plus one, which can start inside a multi-byte character, so
the table reads the text of lexed tokens alone. `filter_collect` writes the table, `sign_fold`
compacts it in place, and `lexfilter::filter` is a view over it, so the FCS
lex-filter differential grades the representation the parser reads.

D5's by-value rebuild on every `peek` cost 150 instructions per byte (the
productions peek the token at the cursor dozens of times per token), so the
productions read through `parser::filtered_cursor::FilteredCursor` instead: it
rebuilds the token at the cursor once per move and lends it out, and its
fields are private to its module, so nothing moves the position without
refreshing the lent token. `peek` returns a reference, as before. Lookaheads
past the cursor rebuild by value; the ones that test a single unit kind compare
`FilteredTokens::kind_at`.

`tests/all/filter_reference.rs` holds the table to the frozen filter directly,
before the sign fold (the parse's lockstep assertion holds it after), over the
lexical pairs plus `FILTER_SHAPES` (type applications, the fused-token splits,
the sign fold, and the keywords whose contexts close with virtuals), over the
`CORE` triples, and over a random walk, each source also with a multi-byte
character before every line break, and checks the pairs reach every kind
rebuilt by a rule of its own. Two planted bugs: dropping the end offset when
the fold compacts the table fails the lexical pairs (through `FILTER_SHAPES`),
the random walk, and the panic sweep's `CORE` triples; rebuilding a promoted `>`
as `Greater(false)` fails the lexical pairs, the random walk and the kind
round-trip, and no panic-sweep case, so `FILTER_SHAPES` is part of this
oracle.

Over the corpus's `src/` tree, min of 3, on the loaded machine:

| phase | instr/B before | instr/B after | alloc-B/B before | alloc-B/B after |
|---|---|---|---|---|
| lex | 83.6 | 83.6 | 4.06 | 4.06 |
| lex-filter | 257.9 | 263.3 | 23.40 | 10.76 |
| parse | 581.8 | 606.1 | 70.09 | 57.45 |

Bytes allocated by the filter fall by 54%, and by the whole parse by 18%.
Instructions rise by 4%: the filter converts each token it emits to a kind
(+5, gone when the filter emits kinds, Stage 4), the cursor rebuilds each
token once, and the lookaheads past the cursor rebuild what they read.

### Stage 4: the filter reads the raw table by index

**Dependencies**: Stage 3. **Implements**: D4.

`Filter` takes `&RawTokens` instead of an iterator, skips trivia by kind, and
its `TokenTup` loses the `Token` payload; `ODUMMY` carries a `TokenKind`.

**Oracle**: as Stage 3. `parse_counts`: the filter's allocations per byte
fall to the table's own growth.

**Result.** `lexfilter::filter_collect` and `lexfilter::context_pushes` take
`&RawTokens`; `Filter::pull_raw` walks the table by index and skips trivia,
directive lines and inactive code by kind. A `TokenTup` is `Copy`: a
`TokenContent` that names a `TokenKind` (with the typar promotion of `<` and
`>` as variants of their own, `TyparLess` and `TyparGreater`, and an
`ODUMMY` wrapping a `DummyInner` rather than a boxed `Token`), a `u32` span
and two `Pos`: 32 bytes (asserted at compile time) against 56. The filter
writes `FilteredKind`s straight into the table. The few rules that read a token's text (operators,
the `1..` split, the typar scan's fused closers) read `source[span]`, which
every lexed token's span is; `typars_close_op_split` still returns `Token`s,
whose kinds the split keeps. The streaming `lexfilter::filter`, which the FCS
lex-filter differential reads, takes an active-code stream as before and
builds a table from it with `RawTokens::from_active`, which marks each gap
the stream leaves as inactive code.

Two lookaheads allocated a vector per call (`is_long_ident_equals_loop`, run
for most identifiers, and `peek_adjacent_typars`); they reuse a buffer kept in
the filter, which is most of the allocation drop below.

`filter_reference` now also checks each source followed by an aligned sibling
line, so an `OBLOCKSEP` follows whatever token the source ends with, and holds
the filter over `RawTokens::from_active` of the active stream to the filter
over the full-trivia table. The sibling axis is there because a planted bug
passed every oracle without it: updating `last_real_end` only for an
unpromoted token moves the `OBLOCKSEP` after `f<int>` at the end of a line
by one byte (`do\n  f<int>\n  g\n`), and neither a pair nor a `CORE` triple
spells a type application, a line break and a sibling. With the axis the
lexical pairs and the random walk fail on it. A second plant, dropping `-`
from `is_adjacent_prefix_capable_op`, fails the same two.

Over the corpus's `src/` tree, min of 3, on the loaded machine:

| phase | instr/B before | instr/B after | allocs/B before | allocs/B after | alloc-B/B before | alloc-B/B after |
|---|---|---|---|---|---|---|
| lex | 83.6 | 82.9 | 0.0006 | 0.0006 | 4.06 | 4.06 |
| lex-filter | 263.3 | 196.7 | 0.0276 | 0.0012 | 10.76 | 6.26 |
| parse | 606.9 | 535.4 | 0.0363 | 0.0099 | 57.45 | 52.95 |

The filter costs 25% fewer instructions and the parse 12% fewer, now below
the 546 it cost before #542 began. Of the filter's 66, about 46 are the
payload (the conversion at the input and the output, cloning `Token`s through
the delayed stack, the boxes), about 4 the two lookaheads' allocations, and
about 16 the `u32` span, which shrinks what each rule moves. What remains is
the rules themselves and the newline scan (`advance_cursor_to`, about 8% of
the filter's samples), which Stage 5 replaces.

### Stage 5: the newline table

**Dependencies**: Stage 4. **Implements**: D3.

`line_starts` computed once per source; `Filter::advance_cursor_to` becomes a
forward cursor over it, and `Filter::utf16_col` indexes it.

**Oracle**: as Stage 3 (positions decide where virtual tokens go and what the
FS0058 messages say, so the stream and diagnostics comparison covers them),
plus a direct property: the table equals the naive scan, on every generated
input.

**Result.** The table is `tokens::LineStarts`, one `u32` per line: line 1
starts past a file-start BOM, every other line one byte past a `\n`. It is
built by one scan that compares 32-byte chunks at a time and pushes a start
per set bit. `filter_collect` and `context_pushes` build it and hand the
filter a `tokens::LineCursor` over it, which moves only forward (it asserts
so, and that the offset is in the source), so a token boundary costs one step
per line crossed; `Filter::byte_offset` and `Filter::utf16_col` index the
table by line, and the sparse `(line, start)` list and its binary search go.
The filter's `Pos` is the table's `LinePos`.

`tests/all/line_table_reference.rs` holds the cursor to a rescan of the bytes
before each offset: at every offset in turn, from every offset straight to the
end, and at every token boundary of the full-trivia table under symbol sets
that make the `#if` arms active and inactive. Its sources are the lexical pairs
plus `NEWLINE_SHAPES` (a `\n`, `\r\n` and lone `\r` inside a block comment,
each string form and an inactive region, with multi-byte characters beside
them), every placement of one and two breaks in sources up to 257 bytes (so
the chunked scan sees a break at every offset of a chunk and in its tail),
and random character strings and piece walks. `NEWLINE_SHAPES` also joins
`filter_reference`'s pieces, and the filter's own position test
(`lexfilter/position_tests.rs`) now runs over the full-trivia table with
inactive regions, a lone `\r`, multi-byte characters and a BOM, and checks
`byte_offset` and `utf16_col` at every token start.

Five planted bugs in the table or its cursor each fail the rescan property,
the filter's position test and `filter_reference`: counting a lone `\r` as a
break, leaving the cursor on the line before a line start, ignoring the BOM,
and dropping a `\n` in the scan's chunked part or in its tail. A sixth, reading
`byte_offset`'s line start one line early, leaves the table alone, and fails
the position test and `filter_reference` (through the FS0058 columns).

Over the corpus's `src/` tree, min of 3, on the loaded machine:

| phase | instr/B before | instr/B after | alloc-B/B before | alloc-B/B after |
|---|---|---|---|---|
| lex | 83.0 | 83.6 | 4.06 | 4.06 |
| lex-filter | 196.8 | 189.8 | 6.26 | 5.76 |
| parse | 535.7 | 529.3 | 52.95 | 52.45 |

The filter costs 7.0 instructions per byte fewer (4%), and the parse 6.4.
The lexer is unchanged; its 0.6 is noise. A plain byte loop for the scan
saved only 1.1: the old rescan counted `\n`s with a vectorised filter, so
most of the gain is the chunked scan.

### Stage 6: the lexer writes the raw table

**Dependencies**: Stage 1 (parallel to 2–5). **Implements**: D1's "one pass".

A table-writing pass beside `DriverCore`: logos tokens become kinds as they
are produced, without materialising `Token`, `TriviaToken` or a `VecDeque`
entry per token. The iterator drivers stay for the LSP and as the reference.

**Oracle**: the directly written table equals the one Stage 1 builds from the
iterator, over the generated inputs and, in lockstep, every parse.

### Stage 7: measure, then fold the diagnostic scans

**Dependencies**: Stage 6. **Implements**: D6.

Measure the three scans over the raw table. Fold a scan into the lexing pass
only where it costs more than a few instructions per byte.

**Oracle**: the folded diagnostics equal the separate scans' (kept
test-only), over the generated inputs and every parse; `parse_counts` shows
the gain that justified the fold.

### Stage 8: retire the references

**Dependencies**: Stages 5 and 7. **Implements**: the end state.

Delete the frozen filter copy and the lockstep assertions, and decide whether
the iterator drivers become views over the table (then the LSP's swallow-mode
callers read the table). Optional, by profile: move the productions' hottest
`peek` sites to kind comparisons (D5).

**Oracle**: full gate and manifests unchanged.

## Non-goals

- The productions' own cost and `TreeBuilder::finish` (the flat table's).
- Incremental relexing.
- Changing what any pass computes: every stage is a representation change
  held to a reference, and a behaviour change found on the way is its own PR.
