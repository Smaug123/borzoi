# Compile-item fidelity plan (escapes, path aliasing, Update batching)

> **Status.** **F1** (`%XX` escapes) has landed, as stages E0–E4 of
> [`msbuild-escaped-value-plan.md`](completed/msbuild-escaped-value-plan.md):
> item specs are stored `Escaped` and unescaped once at the point of use, on
> the literal fast path and across the glob-resolver seam alike. **F0** (the
> generative compile-order harness) has landed too. The other distinctive
> Compile-item stages are still outstanding: **F2** (alias matching), **F3**
> (batching degrade), **F4** (ratchet).

> Companion to the package-reference certainty work (PR #905, branch
> `packageref-out-of-order-update`). That branch closed three MSBuild
> value-semantics gaps for the *dependency* capture — `%XX` escapes, path
> aliasing in identity comparison, and the lazy evaluator's stranded-prefix
> `Update` batching — by degrading to uncertain at the capture chokepoints.
> The Compile-item subsystem has the same exposure but a different correct
> response, because Compile identities **are paths**: path semantics are the
> modelled domain, so most of these classes should be *modelled exactly*
> rather than declined.

## Background: verified MSBuild facts

All of these were established with `dotnet msbuild -getItem` stub projects
during the package-reference work (see the differential canaries in
`crates/msbuild/tests/fsproj_packageref_diff.rs`), plus the dotnet/msbuild
sources (`LazyItemEvaluator.ComputeItems` / `ProcessNonWildCardItemUpdates`,
`EscapingUtilities`):

1. **`%XX` unescaping.** MSBuild unescapes `%` + two hex digits during
   evaluation (`B%65ta` → `Beta`, `1%2E0` → `1.0`). A bare `%` with any
   other suffix stays literal. Escaped metacharacters are inert: `%3B` is a
   literal semicolon that does *not* split an item list, `%2A` a literal
   star that does *not* glob.
2. **Path-normalised identity comparison.** `Update`/`Exclude`/`Remove`
   specs match items under lexical path normalisation: `./Beta` matches an
   item whose identity is `Beta`, whichever side carries the alias
   spelling. Normal document-order semantics still apply — an ordinary
   `Update` before its `Include` modifies nothing; only the separately
   documented stranded-prefix bug (fact 3) reaches later `Include`s.
3. **Stranded-prefix `Update` batching.** The lazy evaluator batches
   all-literal `Update` fragments into a dictionary keyed by normalised
   path and applies the batch at the next flush point. When batching aborts
   midway through a spec (a fragment collides with an already-batched key,
   or a wildcard/item-reference fragment follows a literal one), the abort
   path removes the batched prefix under the raw-text key — the wrong key —
   so the stranded prefix re-applies at a later flush, *after* `Include`s
   that follow the `Update` in document order. This is an upstream bug we
   deliberately do not mirror.
4. **Escapes apply inside `Condition` attributes too.**
   `Condition="'%74rue' == 'true'"` evaluates *true* (oracle-verified for
   both property-element and item gates). *Landed* as E2 of
   `docs/completed/msbuild-escaped-value-plan.md` — the condition evaluator now models
   escape-bearing condition text rather than committing a wrong `False`.
   Escapes arriving in condition operands via *property values*
   (`<P>%74rue</P>`, `'$(P)' == 'true'`) fall out of the same escaped-domain
   store. The F0 generator should still gain an escaped-condition dimension
   to hold the line once it exists.
5. **Escapes apply inside `$(…)` property expressions too** (oracle
   `expand` op, 2026-07-11). MSBuild unescapes before a property function
   sees its text — literal arguments
   (`$([System.IO.Path]::IsPathRooted('%2fabc'))` → `True`), property
   values spliced into arguments or used as receivers (`$(P.Length)` with
   `<P>a%20b</P>` → `3`), and even escape pairs that only materialise
   when expansion composes them (`'a%$(N)b'` with `<N>20</N>` → the
   function sees `a b`). *Landed* as E3 of the escaped-value plan: the
   property *expression* evaluator declines every escape-bearing string —
   guards at its entry, at the property splice, and on the composed argument
   template, pinned in `property_expr_diff.rs` corners and
   `properties/expr.rs` unit tests. (These are the guard points
   cross-referenced from `docs/completed/sdk-chain-exactness-plan.md`.) The *plain*
   splice (`$(P)` outside any function chain) and `%XX` in literal
   property-value text now unescape correctly under the escaped store.

## Current state of the Compile subsystem

- General `<Compile Update=…>` and `<Compile Remove=…>` already degrade
  (`diagnose_item_op` in `crates/msbuild/src/evaluator/item_pass.rs`
  pushes an unsupported-operation diagnostic under `compile_context`, which
  sets `items_uncertain`). The **one modelled Update** is the metadata-only
  compile-*order* update (`<Compile Update="a.fs" CompileOrder="CompileBefore"/>`
  etc., `apply_compile_order_update` — the trigger is a `CompileOrder`
  metadatum, whose value names the order slot), which moves
  already-captured items between order buckets.
- Include paths are captured as `project_dir.join(entry)` after `$(…)`
  expansion, `%XX` decode (`fragment_identity`, F1), and `\`→`/`
  normalisation, with `.`/`..` segments left intact. Compile-order Update
  matching (`take_matching_compile_items`) compares those `PathBuf`s with
  **exact equality** — no lexical `.`/`..` normalisation (F2's remaining
  gap; an alias-spelled `./a.fs` still silently fails to match include
  `a.fs`).
- Globs, item references, and metadata references in includes and
  compile-order update targets already degrade (diagnostics under
  `compile_context`).
- Differential coverage: `fsproj_msbuild_diff.rs` compares static
  `Compile`/`CompileBefore`/`CompileAfter` lists via `-getItem` and the
  effective F# order via `-target:FSharpSourceCodeCompileOrder`, with
  `fs::canonicalize` on both sides; `fsproj_msbuild_corpus_diff.rs` sweeps
  the corpus. `fsproj_compile_order_generative_diff.rs` (F0) is the generative
  harness: it runs the SDK's own `FSharpSourceCodeCompileOrder` through the
  oracle's `targetItems` op and holds every certain compile order to it.

## Exposure classes and design decisions

| Class | Example | Status | Decision |
| --- | --- | --- | --- |
| Escaped path character | `Include="a%20b.fs"` | **modelled** (F1 decoder landed) | Unescape after split |
| Escaped semicolon | `Include="a%3Bb.fs"` | **modelled** (F1, both seams) | Split raw, then unescape fragments |
| Escaped wildcard | `Include="a%2Ab.fs"` | **modelled** (F1, both seams) | Glob-detect raw, store unescaped |
| Alias in order-Update target | `Update="./a.fs"` vs include `a.fs` | silently fails to match, certain-wrong order | **F2**: lexically normalised match keys |
| Stranded-prefix batching | `Update="a.fs;a.fs" CompileOrder=…` before the Include | applied to prior items only, certain-wrong order | **F3**: degrade (upstream bug, not worth mirroring) |

Why model where the package side degraded: a package id is an opaque
identifier — `./Beta` is not a legal id, so declining costs nothing. A
Compile identity is a file path — `%20` in a filename, `..\` segments, and
alias spellings occur in real projects, so declining would erode coverage
the LSP actually needs. The exception is the stranded-prefix batching,
which is an upstream defect in *any* item type; there the package-side
decision (degrade + canary that fires if upstream fixes it) carries over.

Open questions to settle **by oracle, before implementing** the remaining
stages (the `msbuild-condition-oracle` skill's stub-project method):

- **Multi-byte escapes.** *Settled from source* by the escaped-value plan:
  `EscapingUtilities.cs:112` decodes each `%XX` pair as a single UTF-16 char
  (`%E2` → U+00E2), *not* byte-wise UTF-8.
- **Case sensitivity of path matching on unix.** MSBuild's comparison
  dictionaries are `OrdinalIgnoreCase` even off-Windows; confirm whether
  `Update="A.fs"` matches include `a.fs` under `dotnet msbuild` on this
  platform, and mirror exactly (needed by F2).
- **Trailing dots/spaces** in fragments are trimmed only on Windows —
  expected out of scope (unix hosts), but pin one case to document it.
- **`Exclude` interaction with the glob-resolver seam**: the escaped half
  is settled. Excludes cross the seam as escaped `SpecFragment`s, and the
  resolver applies one as MSBuild does, which depends on whether the include
  it meets is a literal or a glob (the escaped-value plan's "Filespec
  classification", pinned by oracle). Alias-spelled excludes (`./a.fs`) are
  still open and belong with F2.

## Staged implementation

Implement each remaining stage on its own branch, stacked as necessary, so
that a reviewer can review each branch in isolation.

### Stage F0: generative compile-order differential harness

**Status**: landed, as `crates/msbuild/tests/fsproj_compile_order_generative_diff.rs`.
Over the restricted space it found no divergence: 12,000 random cases, and
every planted evaluator bug tried (a case-sensitive bucket name, a bucketless
value kept, the first `CompileOrder` child winning, a rooted `Update` target
ignored) caught with a shrunk witness. Two departures from the sketch below:
the oracle runs the target in-process (`targetItems`) rather than a
`dotnet msbuild` per case, and the target's text is lifted from the installed
`Microsoft.FSharp.Targets` rather than imported with it, since that file
imports `Microsoft.Common.targets`. Rare *boundary* items (`Remove`, an
`Update` writing other metadata, `Exclude`, `@(Compile)`, an
`ItemDefinitionGroup` default) ride along, held to the same property.
**Dependencies**: none.

**Implements**: the "no generative harness" gap (Current state, last bullet).

Mirror `fsproj_packageref_generative_diff.rs` for the Compile capture:
generate small SDK-style projects from index pools — literal and
`$(…)`-valued includes across `Compile`/`CompileBefore`/`CompileAfter`,
metadata-only compile-order Updates (a `CompileOrder` metadatum whose
*value* is one of the order slots, e.g. `CompileOrder="CompileBefore"`;
a `CompileBefore`/`CompileAfter` metadata *name* on an Update is inert for
ordering — `apply_compile_order_update` and the F# target read only
`CompileOrder`, so generating those would produce no-op cases that let the
harness pass without exercising the modelled path),
defined/undefined-property conditions, SDK/project
group interleavings — lay them out under a synthetic `MSBuildSDKsPath` SDK
that imports the real F# ordering targets (or the fixture SDK plus the
`FSharpSourceCodeCompileOrder` invocation already proven in
`fsproj_msbuild_diff.rs`), and assert **certain ⇒ exact**: whenever
`items_uncertain == false`, our ordered Compile list equals MSBuild's
effective order. The generated space starts *inside* today's modelled
domain: no escapes, no alias spellings, no duplicate update targets — each
later stage flips its dimension on. Include the two packageref-harness
sanity companions: a deterministic certain-fraction lower bound, and a
hand-built known-certain canary that must reach the oracle.

**Correctness oracle**: the harness itself is the oracle. Green run over
the restricted generator space (fixing any divergences it already finds in
that space is in scope for this stage); `most_generated_cases_are_certain`
analogue bounds silent skipping; the canary pins the plumbing.

---

### Stage F1: model `%XX` unescaping for item paths — landed

**Status**: landed, as stages E0–E4 of
[`msbuild-escaped-value-plan.md`](completed/msbuild-escaped-value-plan.md).
Item specs are stored `Escaped`; `push_include_entry`,
`route_item_through_resolver`, and `resolve_compile_update_targets` decode
identities and match targets via `fragment_identity` / `scalar_use`; `Link`
metadata takes the same decode. At the glob-resolver seam,
`GlobRequest` carries `SpecFragment`s — split, still escaped, classified on
their escaped text — and the resolver decodes each at its own point of use,
so an escaped `%3b` never re-splits and an escaped `%2a` never globs. The
seam property (fragment count and literal/glob classification preserved
end-to-end) is `with_imports_tests/globs.rs`'s
`route_splices_resolver_output_verbatim`; MSBuild-faithfulness, including the
two ways an `Exclude` applies, is `fsproj_item_escape_generative_diff.rs`.

---

### Stage F2: path-normalised matching for compile-order Updates

**Status**: outstanding. **Dependencies**: F0.

**Implements**: alias row of the decision table.

Oracle-pin case sensitivity first (open question above). Introduce a lexical
normalisation (component-wise `.`/`..` resolution over the already
project-dir-joined path, no filesystem access) used as the *match key* in
`take_matching_compile_items` and for `Remove`-style comparisons the
resolver seam performs on literals; captured identities keep today's
display form (the differential already canonicalises for comparison). A
`..` that escapes above the root, or any component the normaliser cannot
resolve lexically, degrades via the existing diagnostics channel.

**Correctness oracle**: property test — for all generated (include
spelling, update-target spelling) alias pairs, match-iff-MSBuild-matches
(reference: the oracle-pinned normalisation rules); differential fixtures
(`./a.fs`, `sub/../a.fs`, backslash spellings); generator dimension "alias
spellings" flipped on in F0 and green.

---

### Stage F3: degrade stranded-prefix compile-order Updates

**Status**: outstanding. **Dependencies**: F1 (decoder) and F2 (the
duplicate check must run on unescaped, normalised keys — landing it earlier
would repeat the packageref review cycle where raw-text comparison missed
escaped/aliased duplicates).

**Implements**: batching row of the decision table.

Port the package side's duplicate-update degrade (the
`PackageReferenceUncertaintyCauseKind::DuplicateUpdateIdentity` model) to
the compile-order Update path, with the duplicate test on normalised keys:
within one `<Compile Update>` spec, a fragment that repeats an earlier
fragment's key (or a non-literal fragment after a literal one) marks
`items_uncertain` with a dedicated `CompileItemUncertaintyCauseKind`
variant. Document the intentional decline exactly as on the package side.

**Correctness oracle**: fail-first unit tests; a differential canary
pinning MSBuild's out-of-order application for a duplicated
`<Compile Update>` (fails if upstream fixes the wrong-key removal, so the
degrade can be reconsidered); generator dimension "duplicate/aliased
update targets" flipped on in F0 and green.

---

### Stage F4: full-space ratchet

**Status**: outstanding. **Dependencies**: F1–F3.

All generator dimensions on simultaneously (interactions between escapes,
aliases, and duplicates are exactly what generative testing is for);
certain-fraction bound re-tuned for the enlarged space; corpus sweep and
`fcs`-side gates re-run. Any divergence found here is a bug in an earlier
stage's model, fixed under this stage.

**Correctness oracle**: the F0 harness over the full space, the corpus
gates, and the workspace test suite — all green.
