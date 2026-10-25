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
or not FCS kept anything there. The offside column is read off the source
layout: the column of the first token after the module's header (a named
module's name, a nested module's `=`). Anchoring on our own first declaration
instead would let that declaration, when it is itself debris, align with
itself.

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
  after a `.` where it ends an access (`xs.Length` → `xs.`), every declaration
  wholly *after* the damage must survive too, shifted. Two token deletions and
  two member deletions are drawn per file; `BORZOI_RECOVERY_SOAK=all` runs
  every token of every file instead (a long soak, not a gate).
- **`recovery_sweep::recovered_trees_match_fcs_under_deletion`** samples one
  file in twelve whose clean trees already agree with FCS. It grades one token
  deletion and one member deletion per file against FCS, and pins each case in
  `crates/cst/tests/manifests/recovery_sweep.txt`.

## What remains

These were measured on 2026-10-25. Of the 731 files that one parser or both
reject, 268 are `exact`, 334 are `outside-damage`, and 129 are `divergent`. Of
the 584 deletion cases against FCS, 98 are `divergent`, all in the token
family. Every member-name deletion is `exact`.

The largest class of divergence is the remains of a construct our parser
abandoned, parsed as module-level declarations of their own. When a declaration
parser gives up part-way through a type, the rest of the type's body sits in
offside blocks that no declaration owns, and the module loop makes `Expr`
declarations of the member bodies there. Likewise, after a token that cannot
begin a declaration (`member this.P = 1` outside a type), the rest of the line
becomes a fresh expression declaration. FCS makes a declaration of neither.

Two live defects were found and not fixed. Both are outside recovery proper:

- `Array. [ yield … ]` (a dotted indexer with whitespace and a sequential body)
  is valid F# that our parser rejects. Its recovery then swallows the next
  module (`tests/fsharp/typecheck/sigs/pos30.fs`, `Array.ofList [` with the
  name deleted). The member family therefore excludes a deletion that leaves
  `recv. [` or `recv. (`.
- `ENUM_CASE must contain a value expression`, `TYPAR_DECL must contain an
  IDENT_TOK child`, and about twenty other normaliser assertions panic on
  recovered trees. They cost nothing in the relation, because the units are
  damaged, but they keep those files from being `exact`.

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
