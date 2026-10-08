# Grading recovered trees

An LSP spends most of its time on broken code: a buffer is mid-edit far more
often than it is clean. The clean-file parser differential cannot see that
time at all. It compares trees only where both parsers accept the file, and the
pinned corpus is the F# compiler's own sources, well-formed by construction.
This document describes how recovered trees are graded instead, and what that
grading has not yet closed.

## What is compared

`crates/cst/tests/all/common/recovery.rs` holds the relation. A file is cut into
**units**, the pieces that can be damaged independently:

- each top-level module or namespace *header* (its body elided);
- each module-level declaration, recursively through nested modules. A nested
  module is a unit for its header, and each declaration in its body is a unit
  of its own.

A unit is keyed by where it sits: the chain of enclosing modules, its start
offset, and its FCS case name. Both sides use the FCS-faithful ranges that the
range audit already proves on clean files.

**Damage** is the union of both parsers' error spans. Each span is widened back
to the end of the last significant token before it, and spans are closed
intervals. Both parsers habitually report an error at the token *after* the
damage (FCS's "unexpected keyword `let`" sits on the next declaration's
`let`), so a unit that ends where an error begins counts as damaged.

A unit is **damaged** if its range touches the damage on *either* side. The
two parsers often disagree about where a broken declaration ends (FCS stops a
broken type's range short of the error that broke it), and the relation must
not read that as one side keeping the unit clean.

The verdict, strongest first:

- **`exact`**: the whole normalised trees are equal, recovery placeholders
  included.
- **`outside-damage`**: every undamaged unit is on both sides, under the same
  key and with the same normalised shape, *and* every unit whose start offset
  is undamaged starts on both sides. The second clause is the boundary check.
  The remains of a construct that our recovery abandoned, parsed as
  declarations of their own, each sit next to one of our errors, so the unit is
  damaged; but where the unit begins is not damaged, and FCS begins nothing
  there.
- **`divergent`**: anything else. A unit is missing, misplaced or different.

The relation is symmetric, apart from one exemption. FCS's implementation-file
`recover` arm drops the rest of a module after an error it cannot resynchronise
from. Past that point FCS's tree has nothing to compare against. One of our
units that starts beyond FCS's reach is therefore not graded, but only if it
sits on its module's offside column. The manifest counts these units as
`beyond-fcs:<n>`. A misaligned unit is debris, and stays a divergence whether
or not FCS kept anything there.

A unit that one normaliser does not model is not compared, but it is counted.
Every verdict carries `<compared>/<undamaged>/<total>`, and the manifests pin
all three numbers. So a change that hides a declaration still moves a pinned
line, even when the relation still holds. That includes hiding it by growing
the damage, or by making it unmodelled.

## FCS's recovery nodes

The normalised AST (`crates/cst/tests/all/common/normalised_ast/model.rs`)
models all four of FCS's recovery nodes. The table gives each one's
counterpart in our CST:

| FCS | our CST | projected |
|---|---|---|
| `SynExpr.ArbitraryAfterError` | a missing `Expr` child (a zero-width `ERROR`) | `Error` |
| `SynExpr.DiscardAfterMissingQualificationAfterDot` (`foo.`) | a `LONG_IDENT_EXPR` whose `LONG_IDENT` ends in the dot (`foo.`, `A.B.`), or a `DOT_MISSING_EXPR` wrapping any other receiver (`(f x).`, `foo.Bar(1).`) | `DiscardAfterMissingQualificationAfterDot(receiver)` |
| `SynType.FromParseError` | a required type child left out (`x :`, a field `of int *`) | `FromParseError`, at the slots where FCS's grammar fills the hole |
| `SynExpr.FromParseError` | an unclosed `(` or `begin` (no closer token) | `Paren(FromParseError e)`, or a bare `FromParseError e` for `begin` |
| `SynExpr.FromParseError`, the other productions (a binding right-hand side followed by junk, a `_.` dot-lambda) | none | never built from our side |
| `SynPat.FromParseError` | none | never built from our side |

The two "none" rows are honest gaps. A tree that holds one of them is never
`exact` against ours. The unit that contains it is damaged on both sides,
because FCS reports the error that made the node, so `outside-damage` is
unaffected.

`DOT_MISSING_EXPR` is a CST change. It replaces a `DOT_TOK` that previously
dangled loose in the enclosing node. An identifier-path receiver keeps its old
shape, because that shape is what the member-completion handler anchors on.

## The gates

- **`parser_corpus_diff`** (`crates/cst/tests/manifests/parser_corpus_diff.txt`)
  pins a verdict for every file that either parser rejects, after the file's
  bucket. For example: `… both-reject outside-damage 3/4/5`. A grading
  failure is a hard assertion.
- **`recovery_sweep::deletion_keeps_undamaged_declarations`** (no FCS).
  Delete one token from each clean corpus file and reparse. Every declaration
  wholly before the damage must be in the damaged tree with the same range and
  the same green subtree. For the **member** family, deletion of the member name
  after a `.` where it ends an access (`xs.Length` → `xs.`) or is followed by
  an opening `[` / `(` (`Array.ofList [` → `Array. [`, a dotted indexer), every
  declaration wholly *after* the damage must survive too, shifted. Two token deletions and
  two member deletions are drawn per file; `BORZOI_RECOVERY_SOAK=all` runs
  every token of every file instead (a long soak, not a gate).
- **`recovery_sweep::recovered_trees_match_fcs_under_deletion`** samples one
  file in twelve whose clean trees already agree with FCS. It grades one token
  deletion and one member deletion per file against FCS, and pins each case in
  `crates/cst/tests/manifests/recovery_sweep.txt`.

## What remains

These were measured on 2026-10-08, after the fixes in this change. Of the 731
files that one parser or both reject, 280 are `exact`, 394 are
`outside-damage`, and 57 are `divergent`. Of the 642 deletion cases against
FCS, 48 are `divergent`: 45 in the token family, and 3 member deletions that
leave `recv.()` inside a `try` body (see the `.()` class below).

The divergent files, by cause (each file counted once, by its first divergence):

| count | class |
|---|---|
| 13 | **A `module` or `open` inside a type body.** These are the `Module After …`, `Module Between …` and `Module Inside …` fixtures under `tests/service/data/SyntaxTree/Type/`, plus `E_openInTypeDecl.fs`. FCS closes the type and recovers the nested module, with its body. Our parser ends the enclosing module there instead. |
| 8 | **`we-reject-fcs-accepts` files.** Our parser rejects valid code (an acceptance gap, not a recovery defect), and the recovery from that rejection then drops or misplaces a later declaration. Most are the large `tests/fsharp/core/*/test.fsx` scripts. |
| 7 | **A second module head with no `=`.** Examples are `module A` after the file's own `module` header, and a `module` head after `#indent "off"`. FCS recovers a `NestedModule`. Our parser drops the keyword as an error and makes an expression declaration of the name. |
| 6 | **The file's module header normalises differently** (the first divergence the grader reports; later units may differ too). Not yet investigated. |
| 4 | **FSharp.Core sources** (`Query.fs`, `prim-types.fs`, `array2.fs`, `nativeptr.fs`). Not yet investigated. |
| 19 | A tail of single cases. |

The recovery fixes in this change:

- **Remains of an abandoned construct no longer become module-level
  declarations.** When a declaration parser gives up part-way through a type,
  the rest of the type's body sits in offside blocks that no declaration owns.
  The module loop now treats an expression start inside such a block
  (block depth > 0) as debris, and does not let an `OBLOCKEND` at that depth
  close a nested module's body. Before the fix, this was the largest class of
  divergence: member bodies spilled out as `Expr` declarations.
- **The rest of a line after a stray token is debris.** `member this.P = 1`
  outside a type no longer makes `this.P = 1` an expression declaration.

A dotted indexer's body is FCS's `typedSequentialExpr`, so
`Array.[⏎ yield 1⏎ yield 2⏎ ]` (with any spacing around the `.`) is one
`DotIndexedGet` whose index is a `Sequential`.
`parser_diff_dot_index_spacing` grades that construct under every spacing of
its `.`, in four contexts, against FCS: verdicts and accepted trees must agree
outright, and the recovered trees that diverge are pinned cell by cell in
`crates/cst/tests/manifests/dot_index_spacing.txt`. They fall into three
classes, none of them fixed:

- **`recv.()` in an argument.** `.()` lexes as one operator-name token in both
  lexers. FCS reports it and keeps the enclosing parenthesis
  (`f(arg = xs.())` is `Paren(FromParseError …)`); our parser loses the
  parenthesis and swallows the next module.
- **The rest of a `try` body after a broken statement.** When a statement in a
  `try` body fails to parse (`let y = xs⏎ .[i]` there, or `recv.()`), the
  body's remaining statements become module-level declarations. FCS drops
  them with the `try`. Three member deletions in `recovery_sweep.txt` are this
  class.
- **Contents of a bracket the parser abandoned.** In `let y = xs.⏎ [⏎ yield
  1⏎ yield 2⏎ ]`, with the bracket's body offside, the stray `[` opens no
  offside block, so the debris rule (block depth > 0) does not see that
  `yield 2` is inside it, and it becomes an expression declaration.

One other live defect is outside recovery proper:
`ENUM_CASE must contain a value expression`, `TYPAR_DECL must contain an
IDENT_TOK child`, and about twenty other normaliser assertions panic on
recovered trees. They cost nothing in the relation, because the units are
damaged, but they keep those files from being `exact`.

`g f(x)`, an argument that ends in an adjacent application (and anything
built on one: `g f(x).Foo`, `g f(x).[i]`, `g -f(x)`), is FS0597 in FCS's
grammar: `argExpr` reports it on an `atomicExpr` whose high-precedence flag
is set, and keeps the tree. Our parser reports it too, at FCS's span, and
`parser_diff_successive_args` grades the rule as a generated matrix.

## The whole-project gate

`corpus-diff` skips a project outright when FCS reports any error in it
(`fcs_error_skip_reason`). Grading resolution through broken files is a
different problem from grading parse trees. FCS type-checks a file with a
parse error unreliably: one malformed line silences FCS's diagnostics for
every other line of the file, so FCS's *diagnostics* there are no
type-checking oracle. Its *symbol uses* may still be usable. Changing the skip
would take three things:

1. An oracle request that reports parse and check diagnostics separately per
   file.
2. A per-file split, so that a project whose only errors are parse errors in
   some files grades the other files as usual.
3. A use-level analogue of this document's relation for the damaged files
   themselves, comparing only uses outside the damage, with the same widening.

None of that is attempted here.
