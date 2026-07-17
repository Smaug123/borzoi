# `#line` directive span remapping (`LineDirectiveStore`)

> **Status (2026-06-28): Stages 1–4 landed.** This is the dedicated plan
> document referenced by `docs/ifdef-plan.md`'s third deferred follow-up
> ("`#line` effects on diagnostic spans"). It is scoped *core-first*: the pure
> capture/remap core landed (Stage 1 #211, Stage 2 #213), wired into
> same-file line remapping (Stage 3 #219). Cross-file (different-URI)
> relocation was the final stage; its design questions were originally
> gated and are resolved below (Stage 4), with the work split into the pure
> grouping core (4a, #229) and the imperative publish-by-URI shell (4b, #233).
> The only remaining work is the explicitly-deferred *inverse* (virtual →
> generated) map for request handlers (Q4), which warrants its own
> follow-up.
>
> **Implementation note (divergence from the sketch below).** 4b landed as a
> pure `publish` module with `PublishState::plan` / `plan_close` methods (not
> the free `plan_publishes` function sketched in Stage 4b), plus a `pull`
> module for the LSP pull-diagnostics path (where cross-file groups are
> deferred, not reported). Stage 3's `source_diagnostics` /
> `apply_line_directives` were **deleted** by 4b — their same-file behaviour
> is subsumed by the 4a `None` group. The remaining symbols live in
> `crates/lsp/src/diagnostics.rs` (`grouped_diagnostics`, `FileDiagnostics`,
> `group_by_line_directives`) and `crates/cst/src/directives/line_store.rs`
> (`LineDirectiveStore`).

Implement this plan with each stage on its own branch, stacked as
necessary on previous branches, so that a reviewer can review each branch
in isolation.

## Goal

Honour `#line` directives so diagnostics report against the *virtual*
coordinates the directive establishes, mirroring FCS's
`LineDirectiveStore` / `range.ApplyLineDirectives`. fslex (`.fsl`) and
fsyacc (`.fsy`) generate `.fs` files peppered with `# N "source.fsl"`
pragmas precisely so the compiler reports errors against the hand-written
source, not the generated `.fs`. Today we ignore them: every diagnostic
lands at its real offset in the generated `.fs`.

## Background

### What a `#line` directive means

`#line N`, `#line N "file"`, or the bare-numeric alternate `# N "file"`
asserts: *the next source line is line `N` of `file`*. So a directive
sitting on generated line `d` makes generated line `d + 1` report as
virtual line `N`, and a span on generated line `L > d` reports as virtual
line `L + (N - (d + 1))`. Columns are carried through unchanged.

### What already exists

- The recogniser parses the payload: `Directive::Line { number: u32, file:
  Option<String> }` (PR #157), with round-trip PBTs in
  `crates/cst/src/directives/line.rs`.
- The driver currently classifies `#line` as trivia
  (`Directive::is_trivia`) and **swallows** it in
  `crates/cst/src/directives/driver.rs` (`handle_directive_result`):
  it advances `self.pos` past the directive, invalidates the lexer, and
  emits no token. The payload is dropped.

### FCS reference

- `src/Compiler/SyntaxTree/LexerStore.fs` — `LineDirectiveStore` is a
  per-lexbuf `ResizeArray<int * (FileIndex * int)>`: a list of
  `(generatedLine, (fileIndex, virtualLine))` appended in source order
  (hence ascending by `generatedLine`).
- `src/Compiler/Utilities/range.fs` — `range.ApplyLineDirectives()`:
  `findBack` the last directive whose `generatedLine < m.StartLine`,
  compute `xOffset = virtualLine - (generatedLine + 1)`, shift start/end
  lines by `xOffset`, swap in the directive's `fileIndex`, keep columns.
  Applied at diagnostic-formatting time
  (`CompilerDiagnostics.fs`), not at lex time.

We mirror the *data structure* and the *remap arithmetic* exactly so the
core can be differential-tested against FCS later; we diverge on
*application*, because LSP's diagnostic model differs from FCS's (see
below).

### Why the LSP case is harder than FCS's

1. **Spans are byte offsets.** Diagnostics carry `Range<usize>` until the
   late `offset_to_position` conversion in `crates/lsp/src/position.rs`
   (which counts newlines + UTF-16 units from the start of the buffer).
   `#line` remapping is a *line-number* transform, and we don't have the
   virtual file's bytes — so it operates on the converted `Position`
   (line/col), shifting the line and keeping the column, exactly as FCS
   keeps columns.
2. **`publishDiagnostics` is per-URI.** An LSP `Diagnostic` carries only a
   `range`, never a URI; the server publishes a vector of them *for one
   document URI*. A same-file line shift fits this model (just renumber
   the line). Reporting against a *different* file (`# N "other.fsl"`)
   does **not** — it means publishing under another URI, which is a
   publish-model change, not a span tweak. This is the core/cross-file
   split that shapes the staging.
3. **Two diagnostic producers.** `diagnostics_for` (preprocessor) and
   `parse_diagnostics` (parser) both convert spans
   (`crates/lsp/src/diagnostics.rs`). Both must apply the *same* remap, so
   the store is built once per buffer and shared.
4. **Only active-branch directives count.** A `#line` inside a dead `#if`
   branch is never seen by FCS's lexer and must not take effect. The
   driver already only calls `handle_directive_result` in active mode and
   ignores trivia while skipping — so capturing at that hook is correct by
   construction, but the store must come from the driver pass, *not* a
   naive line sweep that would wrongly include dead-branch directives.

## Design of the core

A new pure module in `crates/cst` (alongside `directives/`), with no
dependency on rowan/logos beyond what `directives` already uses:

```rust
/// One `#line` directive that took effect in an active branch.
pub struct LineDirective {
    /// 0-based line in the generated source carrying the directive.
    pub generated_line: u32,
    /// Virtual line number asserted for `generated_line + 1`.
    pub virtual_line: u32,
    /// Virtual file, if the directive named one. `None` => same file.
    pub file: Option<String>,
}

/// Directives in source order (ascending `generated_line`).
pub struct LineDirectiveStore {
    directives: Vec<LineDirective>,
}

pub struct Remapped {
    pub file: Option<String>,
    pub line: u32,
}

impl LineDirectiveStore {
    /// Map a 0-based generated line to virtual coordinates, or `None` if no
    /// directive precedes it (caller keeps the generated coordinates).
    pub fn remap(&self, generated_line: u32) -> Option<Remapped>;
}
```

Indexing (0- vs 1-based) and the strict-`<` boundary are the load-bearing
correctness details; they get pinned by the reference oracle in Stage 2.

## Implementation plan

### Stage 1 — Capture active-branch `#line` directives into the store (done, PR #211)

**Dependencies**: none (builds on the existing `Directive::Line`
payload).

**Implements**: the `LineDirectiveStore` / `LineDirective` types and
their construction. In `driver.rs`, where `handle_directive_result`
currently swallows a `Directive::Line`, additionally push a
`LineDirective` (deriving `generated_line` from the directive's byte
range via a line index over the source). Keep the swallow behaviour
otherwise unchanged — no new token, no CST change, no new `SyntaxKind`
(that is the *separate* `HashLine`/`WarnDirective` follow-up, deliberately
kept decoupled). Expose the accumulated store to callers of
`lex_with_symbols` (e.g. a `Driver::line_directives()` accessor drained
after iteration, or a sibling entry point returning both streams and the
store).

This stage's output is unused by any consumer — dead code, justified by
its tests.

**Correctness oracle**:
- PBT: capture is total — never panics on arbitrary input.
- PBT: only active-branch directives are captured. Generate
  ifdef-wrapped sources with `#line` in both the live and dead branches;
  assert dead-branch directives are absent and live ones present.
- PBT vs naive reference: for ifdef-free inputs, the captured set equals a
  naive line sweep that records every `# N "f"` / `#line N`.
- Example tests: bare `# N "f"`, `#line N`, `#line N "f"`, and a directive
  as the last line of the buffer.

### Stage 2 — `LineDirectiveStore::remap` + reference oracle (done, PR #213)

**Dependencies**: Stage 1.

**Scope**: pure arithmetic in `crates/cst/src/directives/line_store.rs`;
no LSP wiring (that is Stage 3). Adds the `Remapped` type and the `remap`
method. Consumer-less except its own tests — dead code justified by the
oracle, exactly as Stage 1.

**API**:

```rust
pub struct Remapped {
    pub file: Option<String>,
    /// 0-based, ready to drop into an LSP `Position.line`.
    pub line: u32,
}

impl LineDirectiveStore {
    pub fn remap(&self, generated_line: u32) -> Option<Remapped>;
}
```

**Semantics, pinned against FCS** (`src/Compiler/Utilities/range.fs`
`ApplyLineDirectives`; `src/Compiler/SyntaxTree/LexerStore.fs`
`SaveLineDirective`):

- *Boundary (strict).* `remap(L)` is `Some` iff some directive has
  `generated_line < L`. The directive's own line and everything before it
  return `None` (the caller keeps generated coordinates). This mirrors
  FCS's `m.StartLine > directiveLine`; converting FCS's 1-based `>` into
  our 0-based store yields exactly `directive.generated_line < L`.
- *Selection.* The *last* (largest `generated_line`) directive below `L`
  wins, matching FCS's `findBack`. The store is ascending, so
  `partition_point(|d| d.generated_line < L)` gives the count of
  candidates: `None` when `0`, else the directive at `count - 1`. O(log n).
- *Arithmetic (0-based in, 0-based out).*
  `remapped = L + virtual_line − generated_line − 2`, computed in `i64`
  and clamped to `[0, u32::MAX]`. `file` is the selected directive's
  `file.clone()`.

  Note this **diverges from the naive 1-based formula** the earlier sketch
  carried (`offset = virtual_line − (generated_line + 1)`,
  `shifted = query + offset`): FCS works in 1-based lines throughout,
  whereas our `generated_line` is 0-based and `virtual_line` is the literal
  `N` (1-based human intent, stored verbatim as FCS stores it). Converting
  both gives `L + (N − 1) − (generated_line + 1)`. *Check*: a `#line 100`
  on generated line `d`, queried at `d + 1` (the next line), yields `99`,
  which an editor displays as line 100.
- *Underflow.* Only `#line 0` (including the parse-overflow→0 case) can
  drive the result negative; the clamp pins it at line 0. With the strict
  boundary, every other case is `≥ virtual_line − 1 ≥ 0`.

**Correctness oracle**:
- PBT vs naive reference: `remap(L)` equals an independent `remap_ref`
  (linear find-last-before + `i64` arithmetic + clamp) for all `L` over
  arbitrary stores.
- PBT: `None` iff no directive has `generated_line < L`; otherwise the
  result's source directive is the max-`generated_line` one below `L`.
- Example tests pinning the off-by-one via *semantics* (not FCS's internal
  integers): `#line 100` on generated line 0 → query 1 ⇒ 99, query 2 ⇒
  100, query 5 ⇒ 103; the directive's own line (query 0) ⇒ `None`; two
  directives, latest wins; `file` carried through (and `None` for bare
  `#line N`); `#line 0` clamps to 0.
- *Optional / deferred* (as Stage 1 of `ifdef-plan.md` deferred its FCS
  diff): differential test against FCS `range.ApplyLineDirectives` for a
  small corpus, once a `pp-dump`-style shim exists. Not on this stage's
  critical path.

**Forward note for Stage 3** (not built here): a multi-line diagnostic
should take the delta from its *start* line's directive and apply it to
both ends (FCS shifts start and end by the start's `xOffset`). Returning
the absolute line lets Stage 3 recover the delta as `new_start − old_start`,
so no extra API is needed now.

### Stage 3 — Same-file line remapping in the diagnostic producers (done, PR #219; superseded by Stage 4b)

**Dependencies**: Stage 2.

**Implements**: a single composed entry point in
`crates/lsp/src/diagnostics.rs` that wires the Stage 1/2 core into the two
existing producers, applying same-file remaps after `offset_to_position`
has already turned spans into `Position`s.

```rust
pub fn source_diagnostics(text: &str, symbols: &HashSet<String>) -> Vec<Diagnostic> {
    let mut diags = diagnostics_for(text, symbols);
    diags.extend(parse_diagnostics(text, symbols));
    apply_line_directives(&mut diags, &line_directive_store(text, symbols));
    diags
}
```

`crates/lsp/src/main.rs`'s F# branch collapses to a single
`diagnostics::source_diagnostics(text, &symbols)` call. `diagnostics_for`
and `parse_diagnostics` keep their signatures and tests untouched — the
remap is layered on top, not threaded through them, keeping the blast
radius to the new function plus a one-line call-site change.

Two pure helpers carry the work:

```rust
/// Drain a preprocessor pass purely to recover the active-branch store.
fn line_directive_store(text: &str, symbols: &HashSet<String>) -> LineDirectiveStore {
    let mut driver = lex_with_symbols(text, symbols);
    for _ in driver.by_ref() {}
    driver.line_directives().clone()
}

/// Same-file `#line` remap, anchored on each diagnostic's start line.
fn apply_line_directives(diags: &mut [Diagnostic], store: &LineDirectiveStore) {
    if store.is_empty() {
        return;
    }
    for d in diags {
        let Some(r) = store.remap(d.range.start.line) else {
            continue;
        };
        if r.file.is_some() {
            continue; // cross-file → Stage 4
        }
        let delta = i64::from(r.line) - i64::from(d.range.start.line);
        d.range.start.line = r.line;
        d.range.end.line = (i64::from(d.range.end.line) + delta).max(0) as u32;
    }
}
```

*Anchor-on-start* is the load-bearing refinement over the per-position
sketch this section originally carried. FCS shifts a span's start *and*
end by the offset computed from the **start** line's governing directive
(`range.fs` `ApplyLineDirectives` applies one `xOffset` to both ends), so
a multi-line diagnostic keeps its height. We mirror that: compute `delta`
from the start line's `remap`, move the start to the absolute remapped
line, and move the end by the *same* `delta` rather than remapping it
independently. Columns (`character`) are never touched, exactly as FCS
keeps columns. Only directives with `file == None` fire; a directive that
names another file leaves the generated position untouched (its virtual
line belongs to another coordinate space, so renumbering in place would be
wrong). We never silently emit a wrong-file position; cross-file
relocation waits for Stage 4.

Honest scoping note: real fslex/fsyacc output uses `# N "file.fsl"` (with
a filename), so this stage rarely fires on real generated code. Its value
is proving the store→`Position` pipeline end-to-end with a clean oracle,
without taking on the publish-model rework. This is the
infrastructure-consumption step, not the payoff.

Performance note: `line_directive_store` adds a third lexer pass over the
buffer (alongside the two `diagnostics_for` / `parse_diagnostics` already
run). Acceptable to start; fold the store-build into one of the existing
passes only if measurement shows it matters.

**Correctness oracle**:
- Integration test: a buffer with `#line 100` (no file) before a syntax
  error reports the diagnostic at the remapped line, not its real line.
- Integration test: a `# N "other.fs"` (cross-file) directive leaves a
  following diagnostic at its *generated* position (no remap) in this
  stage.
- Unit test: a multi-line diagnostic crossing past a same-file directive
  keeps its height (`end.line − start.line`) and both columns.
- PBT on `apply_line_directives`: an empty store is the identity; a
  same-file remap preserves `end.line − start.line` and both `character`
  values and never produces a line `< 0`.
- Regression: existing `diagnostics_for` / `parse_diagnostics` tests stay
  green — a buffer with no `#line` is unaffected.

### Stage 4 — Cross-file publish-by-URI (done)

**Dependencies**: Stage 3.

This is the hard part. It *was* gated pending the four design questions
below; this section now resolves them and splits the work into three
reviewable sub-stages (4a pure core, 4b imperative shell, 4c
documentation-only). The questions and their resolutions:

#### What FCS pins for us

- **The filename is taken verbatim.** `src/Compiler/lex.fsl` resolves a
  `#line`'s file via `FileIndex.fileIndexOfFile f` — the *non-normalized*
  path (`src/Compiler/Utilities/range.fs`): the raw string from the
  directive, with **no** resolution relative to the generated `.fs`. The
  filename in the resulting error message is exactly the directive's
  string.
- **`ApplyLineDirectives` swaps the file index and shifts both ends by one
  `xOffset`, keeping columns** (`range.fs`). Our Stage 1/2 core already
  mirrors the arithmetic; our `LineDirective.file: Option<String>` already
  carries the verbatim string and `remap` already returns it in
  `Remapped.file`. Stage 3 deliberately `continue`s on `file.is_some()`.

So FCS settles the *arithmetic* and *which string to report*, but **not**
how a string becomes a filesystem location — it never resolves one (it
just interns the raw string into a global file-index table). The
URI-resolution policy below is therefore ours to choose.

#### Resolved design questions

- **Q1 — Publish model / path resolution.** The verbatim file string is
  resolved to a `Url` **in the imperative shell, not the pure core**
  (functional-core / dependency-rejection): the core groups diagnostics by
  the *string*; the server (`main.rs`) turns strings into URIs and decides
  where to publish. Resolution rule: an **absolute** path is used as-is; a
  **relative** path is joined onto the **generating document's parent
  directory** (the only anchor we reliably have, and correct for the common
  "`Lexer.fs` generated next to `Lexer.fsl`" layout). A non-`file:`
  generating URI (`untitled:`, `inmemory:`) has no anchor, so its
  cross-file diagnostics are **dropped with a `log_warn!`** — a degenerate
  case (an unsaved buffer carrying fslex-style `#line "x.fsl"`) that
  essentially never occurs. *Documented imprecision*: a tool that emits a
  *project-relative* path (rather than one relative to the generated file)
  will mis-resolve; this is consciously punted — revisit with project-root
  anchoring only if a real corpus shows it matters.

- **Q2 — Cross-URI lifecycle.** `publishDiagnostics` is per-URI and
  stateful: the client shows the *last* set published for each URI, so any
  URI we ever squiggle we must later be able to *clear*. The server tracks,
  per generating document, the cross-file diagnostics it last contributed
  to each target URI, so a recompute can **clear** targets that drop out
  (fixing the error in a generated `.fs` must erase the squiggle it had
  projected into `Lexer.fsl`). When several generators target one file, the
  server publishes the **union** of their contributions for that target —
  last-writer-wins would silently drop a second generator's real
  diagnostics (a correctness violation, gospel §5). On `didClose(G)`, `G`
  contributes nothing → its targets recompute/clear → its entry is dropped;
  we do not stand behind diagnostics whose generating text we no longer
  hold. The union is nearly free once per-generator tracking exists, which
  clearing requires anyway; if review finds it heavy, single-owner-per-
  target with a collision `log_warn` is the documented fallback.

- **Q3 — Column fidelity.** **Mirror FCS: carry generated columns onto the
  virtual line verbatim, accept the imprecision.** Correcting columns would
  need the virtual file's bytes — IO in the core (violates functional-core),
  the file may be unopened, and it diverges from FCS (forecloses future
  differential testing). The virtual line's indentation may differ from the
  generated line's; that small column skew is accepted, exactly as FCS
  accepts it.

- **Q4 — Inverse mapping (virtual → generated).** **Out of scope; explicit
  follow-up.** This stage is diagnostics-only and one-directional
  (generated → virtual). Request handlers (hover, go-to-definition) need
  the inverse map and a decision about which coordinate space a request
  speaks; until that lands, requests on a generated `.fs` keep using
  generated coordinates (correct — the editor has the generated file open,
  not the virtual one). A dedicated follow-up owns the inverse.

#### Stage 4a — Core: partition diagnostics by virtual file (pure) (done, PR #229)

**Scope**: `crates/lsp/src/diagnostics.rs` only; no publish-model change.
Replace the in-place, cross-file-skipping `apply_line_directives` with a
grouping function:

```rust
/// Diagnostics destined for one file. `file == None` is the document's
/// own URI (same-file shift); `Some(s)` is the verbatim file string from
/// the governing `#line` directive, for the shell to resolve to a URI.
pub struct FileDiagnostics {
    pub file: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn grouped_diagnostics(text: &str, symbols: &HashSet<String>) -> Vec<FileDiagnostics>;
```

Each diagnostic is anchored on its **start** line (as Stage 3): look up the
governing directive, shift *both* ends by that one delta (height
preserved), never touch columns. A diagnostic governed by no directive, or
by a `file == None` directive, lands in the `None` group (same-file shift,
identical to Stage 3). A diagnostic governed by a `Some(s)` directive lands
in the `s` group, remapped onto its virtual line. The arithmetic is exactly
Stage 3's; the only change is *routing* a cross-file diagnostic to a group
instead of skipping it.

`grouped_diagnostics` is purely **additive** in 4a:
`source_diagnostics`, `apply_line_directives`, `main.rs`, and every
existing test are left exactly as Stage 3 left them, so **observable
behaviour is unchanged** and the new function is unconsumed (dead code),
justified by its oracle exactly as Stages 1–2 were. Stage 4b switches the
server to consume `grouped_diagnostics` and deletes the then-dead
`source_diagnostics` / `apply_line_directives`. The per-diagnostic remap is
three lines (`store.remap(start)` plus the shift-both-ends), so the brief
duplication between `apply_line_directives` and the grouping function
during the 4a→4b window is negligible — and avoids a window in which
cross-file diagnostics are dropped (the wrapper alternative would have
removed them from `source_diagnostics` before 4b republished them).

Contract of the returned `Vec<FileDiagnostics>`: the **same-file group
(`file: None`) is always element 0** (possibly empty); cross-file groups
follow in **first-appearance order** of their verbatim file string, one per
distinct string.

**Correctness oracle**:
- PBT — *partition*: concatenating every group's diagnostics is a
  permutation of `diagnostics_for ++ parse_diagnostics` (nothing dropped or
  duplicated; multiset equality on the un-remapped diagnostics).
- PBT — *Stage-3 conservativity*: for the pure grouping over arbitrary
  `(diags, store)`, each output diagnostic's group and remapped position
  agree with a single `store.remap(start)` lookup — the same call
  `apply_line_directives` makes — so 4a reuses Stage 3's arithmetic and
  only changes *routing*. Established via the single-diagnostic placement
  property (a one-element input retains its original start line, so the
  expected group/line are computable directly from `store.remap`).
- PBT — *grouping correctness*: each diagnostic's group `file` equals the
  verbatim string of the directive governing its start line (`None` when no
  directive precedes); within each group every diagnostic's new start line
  is `store.remap(start).line`, height (`end.line − start.line`) is
  preserved, and both `character` values are untouched.
- Examples: `#line 10 "Lexer.fsl"` before an error → a single `"Lexer.fsl"`
  group at the remapped line; interleaved same-file `#line N` and
  cross-file `#line N "f"` directives partition into the right groups; a
  multi-line diagnostic crossing a cross-file directive keeps its height.

#### Stage 4b — Shell: publish-by-URI + lifecycle (imperative, testable) (done, PR #233 — see divergence note in Status)

**Scope**: the server, plus deleting the now-dead Stage-3 same-file path
(`source_diagnostics` / `apply_line_directives`) once the server consumes
`grouped_diagnostics`. Today the publish loop lives in the binary
(`main.rs`) and is untestable. Per gospel ("compute a description of what
to do, then do it"), extract the publish *planning* into the library as a
pure function returning a description, leaving the binary to only perform
IO:

```rust
/// Pure: given the server state and the document that just changed,
/// produce every publishDiagnostics notification to send — the changed
/// document's own set, each cross-file target's recomputed union, and an
/// empty set for every target that dropped out (the clears).
pub fn plan_publishes(state: &mut PublishState, changed: &Url) -> Vec<PublishDiagnosticsParams>;
```

`PublishState` (relocated from the binary into the lib so it is reachable
from tests) gains `cross_file: HashMap<Url /*generating*/, HashMap<Url
/*target*/, Vec<Diagnostic>>>`. `plan_publishes`:
1. computes `grouped_diagnostics` for the changed doc;
2. emits the `None` group under the changed doc's own URI (as today);
3. resolves each `Some(s)` group's URI per Q1, building this generator's
   new contribution map (dropping unresolvable groups with a `log_warn!`);
4. for every target in `new_contrib ∪ previous_contrib`, emits the union
   over all generators' contributions (an empty set ⇒ a clear);
5. stores `new_contrib` as the generator's contribution.

`didClose(G)` routes through the same planner with `G` contributing
nothing, then drops `G`'s entry. The binary shrinks to "call `plan_publishes`,
send each param."

**Correctness oracle** (now unit-testable without a live connection):
- *Fan-out*: a generating doc with a cross-file error yields two params —
  its own same-file set and the target URI's set.
- *Clearing*: publish `G` with a cross-file error, then publish `G` with
  the error gone ⇒ the second plan contains an **empty** publish for the
  target.
- *Close*: `didClose(G)` ⇒ the plan clears the target.
- *Union*: `G1` and `G2` both target `T` ⇒ `T`'s published set is the
  union; re-planning `G1` with no error leaves `G2`'s diagnostics on `T`.
- *Resolution failure*: a non-`file:` generating URI drops its cross-file
  group (with a warn) and leaves the same-file set unaffected.
- *Path resolution*: a relative directive string joins onto the generating
  doc's directory; an absolute string is used as-is.
- *Regression*: a buffer with no cross-file `#line` plans exactly one
  param (its own URI), identical to today.

#### Stage 4c — Documentation only (Q3 / Q4)

No code. Record the column-fidelity decision (Q3) and the inverse-mapping
follow-up pointer (Q4) wherever the request-handling work is tracked, so a
future hover/definition implementer does not re-derive them.

*Status:* the Q3 / Q4 decisions are recorded in the "Resolved design
questions" section above. No separate request-handling plan document exists
yet (that work hasn't started), so this document remains their canonical
record until one does; the inverse (virtual → generated) map (Q4) is the
sole remaining follow-up.

## Notes on ordering and scope

- Stages 1–2 are the pure core and can be reviewed without touching the
  LSP crate. Stage 3 was the first LSP consumer. Stage 4 was gated on the
  design questions resolved above; it has since landed (4a #229 / 4b #233).
- Decoupled from the sibling `ifdef-plan.md` follow-up "`HashLine` /
  `WarnDirective` syntax token kinds": this plan never emits a token for
  `#line`; it captures the directive on a side channel and keeps swallowing
  it. If that follow-up lands first, this plan can read the emitted trivia
  token instead of the side channel, but it does not depend on it.
- Performance: deriving `generated_line` per directive via repeated
  `offset_to_position`-style scans is O(n·k). Fine to start; build a
  single-pass line index only if measurement shows it matters.
