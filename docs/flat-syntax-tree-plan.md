# Flat per-file syntax table (replacing rowan's red layer)

> **Status (2026-11-11).** Stage 0 (harness), Stage 1 (the table behind
> rowan's API) and Stage 2 (the parser writes the table) implemented; Stages
> 3–4 open. Tracks
> [#541](https://github.com/WoofWare/borzoi/issues/541).

## Problem

Every typed-AST accessor walks rowan's red tree. rowan's cursor heap-allocates a
`NodeData` for every node or token it hands out (`first_child`,
`next_sibling`, each step of `children()`), reference-counts it, and frees it
on drop. Profiling `resolve_file` and `infer_file` over the corpus's `src/`
tree (259 files, 11.8 MB) on main (edfcd7e5, and again on 7b4a7975 within
2%), with the system allocator:

| phase | wall (min of 3) | time inside rowan (inclusive) |
|---|---|---|
| parse | 360 ms | — |
| `resolve_file` | 2081 ms | 34% |
| `infer_file` | 877 ms | 48% |

The leaf frames are `rowan::cursor::free`, `SyntaxNode::first_child`,
`SyntaxNode::next_sibling` and `next_sibling_or_token`, with no other
contributors: the cost is the red layer itself, not some particular walk. A
full `descendants_with_tokens` walk of every file costs 91 ms on the red
tree against 9 ms on the green tree beneath it.

How many walks sema does is a separate question with separate planned work.
This plan makes every step of every walk cheap, whatever the walk.

## The spike

`SyntaxNode` / `SyntaxToken` were re-implemented over a flat table: one
contiguous preorder array of every node and token per file, each entry
`(kind, parent, next_sibling, prev_sibling, subtree_end, start, end, index)`
as `u16`/`u32`. A handle is `(Arc<Tree>, u32)`. The handles reproduce the
subset of rowan 0.17's API the workspace uses, rowan's edge cases included
(`first_token` stopping at an empty first child, `token_at_offset`'s boundary
split, `covering_element`'s binary search over children), plus its exact
`Debug` output, which the parser tests snapshot. The table was built from the
green tree after parsing, so the parser was untouched. Outside `cst`, the only
edits needed to compile the workspace were `rowan::NodeOrToken` /
`rowan::WalkEvent` import paths.

| phase | rowan | flat, `Rc` | flat, `Arc` |
|---|---|---|---|
| parse (+ flatten) | 360 ms | 416 ms | 421 ms |
| red/flat `descendants_with_tokens` | 91 ms | 10 ms | 16 ms |
| red/flat recursive `children()` | 31 ms | 12 ms | 13 ms |
| `resolve_file` | 2081 ms | 1513 ms (1.38×) | 1527 ms (1.36×) |
| `infer_file` | 877 ms | 544 ms (1.61×) | 559 ms (1.57×) |

The flat handles recover about 80% of the time the profile put inside rowan.
After the swap, no syntax-tree code appears in the profile's top entries.
`push_entity_full_name_from` (58% of `infer_file`) and the resolver's lookups
lead it now, and both are sema's own work.

`Arc` costs 1–3% over `Rc`, and buys a `SyntaxNode` that is `Send + Sync`
straight away, which a snapshot-based server needs (D2).

`crates/sema/examples/phase_timings.rs` (Stage 0) reproduces the rowan column;
the spike's walk microbenchmarks over green and raw flat arrays were
throwaway.

## Design

### D1. One preorder table per file

`Tree { entries: Vec<Entry>, text: String }`, entries in preorder, so a subtree
is the id range `[id, subtree_end)`. Then `descendants` is a linear scan,
`first_child_or_token` is `id + 1` when `id + 1 < subtree_end`, and `node_span`
style "first/last non-trivia token" lookups are a forward or backward scan of a
range. `text` is the concatenation of the token texts, so the lossless
invariant `tree.text == source` holds by construction and token text is a
slice of it.

The entry layout is not frozen by this plan. The spike's is 32 bytes. Stage 2
measured it against a 24-byte entry, `(kind, is_token, parent, subtree_end,
start, end, index)`, which derives the next sibling from preorder (it is
`subtree_end`, while that stays inside the parent's subtree) and the previous
one from the child index. On the build box, over the corpus's `src/` tree,
min of 5, three alternating rounds:

| layout | table bytes per source byte | parse | `resolve_file` | `infer_file` |
|---|---|---|---|---|
| 32-byte entry | 16.8 | 545–574 ms | 2348–2487 ms | 856–915 ms |
| 24-byte entry | 13.8 | 530–551 ms | 2378–2418 ms | 854–870 ms |

The tables include the child index (8 bytes per element) and the text. The
smaller entry is no slower anywhere but the `children()` microbenchmark
(+7%), and is what Stage 2 ships.

### D2. Handles: `Arc<Tree>` now, borrowed later only if measurement pays for it

Stage 1 keeps rowan's handle shape, owned and cheap to clone, so all 390
hand-written accessors and every consumer keep compiling unchanged. That is
what makes the swap one reviewable PR instead of a rewrite. `Arc` rather
than `Rc` makes the tree `Send + Sync` immediately, for 1–3%.

The issue's end state is borrowed typed nodes, `(&'a Tree, NodeId)`: `Copy`,
no refcount traffic. Measured against the spike, what that adds is the
handle's own overhead: about 13 ms per full-corpus `descendants` scan against
~1 ms raw, and nothing measurable for `children()`-style navigation. That is
an estimated 3–8% of resolve and infer at today's walk counts. It is worth
doing only alongside D5, and only if a measurement on one hot walk confirms
it (Stage 4's gate).

### D3. The parser builds the table directly; the green tree goes

The user asked whether the green build can migrate after the red layer. It
can. Stage 1 builds the table from the green tree, costing 25 ms over the
corpus (+7% on parse, about +1% end to end). Stage 2 removes the green build,
which is about 13% of parse and whose node sharing nothing uses.

The one structural difficulty is `TreeBuilder::start_node_at`. It wraps
children that have already been emitted under a new parent, which a
preorder array cannot do in place. Two known shapes solve it:

- an event log with forward-parent links, converted to the table at
  `finish` (rust-analyzer's parser events); or
- an arena linked in arbitrary order, then one O(n) preorder compaction.

Either keeps `TreeBuilder`'s height accounting (`MAX_TREE_HEIGHT`) as it is.
Stage 2 chooses between them by measurement.

### D4. Structural equality replaces green-tree equality

Green-node equality is structural (kind, children, token text), and the
workspace used it as "are these two subtrees the same tree?": the LSP's
language-version shape gate (`crates/lsp/src/semantic.rs`), `recovery_sweep`'s
undamaged-declaration check, the language-version gate tests, and the parse
cache tests. Those become `SyntaxNode::structurally_eq` (allocation-free) and
`SyntaxNode::structure()` (an owned, hashable snapshot for tests that collect
subtrees), both computing exactly the equivalence green equality did. The
other use of `green()`, moving a parsed tree to another thread because rowan's
red tree is `!Send`, disappears: the `Arc` handles are `Send`.

### D4a. Handle identity is the element, not rowan's (green pointer, offset)

rowan's `SyntaxNode`/`SyntaxToken` `Eq`/`Hash` compare the green pointer and
the offset. The node cache deduplicates green tokens, so two zero-width tokens
of one kind at one offset (adjacent virtual tokens) are *equal* in rowan,
although they are distinct elements. The flat handles compare (tree, id),
which is the element itself. Nothing in the workspace depended on the
collision (every suite and corpus manifest is unchanged); the reference
differential compares results by child-index path for this reason.

### D5. Accessors are generated (with the borrowed handles, if those happen)

`tools/astgen` already generates the dispatch enums and their variant sets.
`docs/ast-versioning-plan.md` D5 wants it to own accessor signatures too, so
that a frozen facade freezes them. The 390 positional accessors only need
rewriting if the handle type changes (D2). So the generator takes them over
in the same stage that moves to borrowed handles, and not before.

### D6. rowan leaves the dependency graph

After Stage 2 rowan is only a test-only reference implementation (Stages 1–2's
oracles). `TextRange` / `TextSize` come from `text-size` (rowan's own
dependency), re-exported from `borzoi_cst::syntax`. The re-export already
exists on the spike, and consumers' `use rowan::TextRange` lines move to it.
rowan, and the oracle tests that compare against it, go away together once the
rowan-shaped API is no longer the contract (Stage 3).

## Implementation plan

> Implement this plan with each stage on its own branch, stacked as necessary
> on previous branches, so that a reviewer can review each branch in isolation.

### Stage 0: phase-timing harness

**Dependencies**: none. **Implements**: measurement for every later stage, and
for the separate walk-count work.

An example binary in `borzoi-sema` (system allocator, unlike the test binaries'
counting `LimitedAlloc`): parse, resolve and infer over `$BORZOI_CORPUS/src`
with the BCL env, min-of-N per phase, plus two tree-walk microbenchmarks
(`descendants_with_tokens`, recursive `children()`). Not a CI gate: wall time on shared runners is noise (see the CI timing
memory), so the harness reports and nobody asserts on it.

**Oracle**: it runs to completion under `nix develop` and reproduces the
baseline table above within noise.

### Stage 1: flat table behind rowan's API, built from the green tree

**Dependencies**: Stage 0 (to show the win). **Implements**: D1, D2 (`Arc`),
D3's "from green" half, D4.

`borzoi_cst::syntax::{SyntaxNode, SyntaxToken, SyntaxElement, NodeOrToken,
WalkEvent, TokenAtOffset, Direction, SyntaxText, TextRange, TextSize}` become
our own types (the spike's `syntax/flat.rs`). `Parse::root` is built from the
green tree after the parse. Consumers change only import paths.
`root.green()` equality becomes `structurally_eq`.

**Oracle**:
- *Reference differential* (new, the main one): for every corpus file and for
  generated token soup (`parser_panic_sweep`'s generator), build both rowan's
  red tree and the flat table from the same green tree, and check that every
  public method agrees on every element: `kind`, `text_range`, `parent`,
  `index`, `children`, `children_with_tokens`, siblings in both directions,
  `first/last_child(_or_token)`, `first/last_token`, `next/prev_token`,
  `descendants(_with_tokens)`, `preorder(_with_tokens)` (including
  `skip_subtree` at every Enter), `token_at_offset` at **every** offset
  `0..=len`, `covering_element` and `child_or_token_at_range` over every
  token-boundary range and every empty range, `Debug` (both forms) and
  `Display`. Equality of handles is identity, so the comparison maps each
  side's elements to `(kind, range, preorder index)`.
- Every existing test passes unchanged, including the parser `{:#?}`
  snapshots, the FCS differentials, and every exact corpus manifest
  (`parser_corpus*`, `recovery_sweep`, `resolve_corpus_diff`,
  `infer_corpus_diff`, the whole-project gate) with **zero** lines moved.
- `Send + Sync` asserted at compile time for `SyntaxNode` / `SyntaxToken` /
  `Parse`.

### Stage 2: the parser builds the table directly

**Dependencies**: Stage 1. **Implements**: D3, D1's layout choice.

`TreeBuilder` emits into the flat table (events or arena, by measurement);
`GreenNodeBuilder` leaves the production path.

**Oracle**:
- Reference differential: the directly built table is structurally equal to
  the one Stage 1 flattens from the green tree, over the corpus, the parser
  test inputs, and generated token soup (the green builder stays as test-only
  reference code).
- `parser_tree_height` / `walk_stack_bound` unchanged: the height bound
  still holds and drop no longer recurses at all.
- Stage 0 harness: parse time below the rowan baseline (360 ms); the harness
  gains a table-memory line (bytes per source byte) so the layout choice is
  measured.

**Result.** `syntax::flat::Builder` records each element in postorder as it
finishes, so a wrap (`start_node_at`) moves nothing: the subtrees of the
elements awaiting a parent are always the tail of the record. `finish`
permutes it into preorder in one pass, an element's preorder id being its
postorder subtree start plus its depth. That is neither of D3's two shapes,
and simpler than both. The green builder survives behind the test-only
`rowan-reference` feature, which drives it in lockstep and asserts on every
parse that the two tables are identical. So every test in the workspace that
parses holds Stage 2 to the oracle, not just the reference differential.
Three alternating rounds on the build box, min of 5 each, with the box
loaded by other work (load 11–24), so read the columns against each other:

| phase | rowan (Stage 0) | Stage 1 | Stage 2 |
|---|---|---|---|
| parse | 602–641 ms | 674–689 ms | 557–600 ms |
| `resolve_file` | 3284–3340 ms | 2571–2800 ms | 2702–2809 ms |
| `infer_file` | 1398–1462 ms | 934–960 ms | 950–1017 ms |

Parse is now below rowan's, and resolve and infer keep Stage 1's gain.

### Stage 3: drop rowan

**Dependencies**: Stage 2. **Implements**: D6.

`text-size` replaces rowan's re-exports; the rowan reference tests are deleted
along with the dependency. Do this when no further stage needs rowan as an
oracle. If Stage 4 is taken, after it.

**Oracle**: `cargo tree -i rowan` is empty; full gate green.

### Stage 4 (gated): borrowed typed nodes with generated accessors

**Dependencies**: Stage 2. **Implements**: D2's end state, D5.

**Gate first**: convert the hottest whole-file walks (today
`infer::project_type_paths` / `project_type_schemes` and the prescans in
`resolve_file`) to a borrowed `NodeRef<'_>` in a throwaway branch and measure
with the Stage 0 harness. Proceed only if the gain is worth a facade-wide
signature change. If it is, migrate one generated category at a time (types,
patterns, expressions, declarations: the same split astgen's PR D1–D4 used),
with an owned↔borrowed bridge so each category's PR compiles alone, and
delete the owned handle when the last category lands.

**Oracle**: per category, the generated accessors pass the accessor coverage
check and every corpus manifest unchanged; astgen's `up_to_date` test covers
the generated signatures.

## Non-goals

- Reducing how many times sema walks a tree (separate planned work; Stage 0's
  harness serves it too).
- Incremental reparsing. Nothing uses rowan's structural sharing today, and a
  per-file table is rebuilt in the time the green tree took.
