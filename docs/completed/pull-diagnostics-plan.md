# Pull diagnostics plan (LSP)

> Adds the LSP 3.17 **pull** diagnostic model (`textDocument/diagnostic` and
> `workspace/diagnostic`) alongside the existing **push**
> (`textDocument/publishDiagnostics`) path. Captured decisions before
> implementation so future work could resume from a cold pickup. Companion to
> [`line-directive-remap-plan.md`](line-directive-remap-plan.md) (the `#line`
> cross-file relocation this reuses) and
> [`fsproj-consumption-plan.md`](../fsproj-consumption-plan.md) (the Compile
> `items` Stage 2 enumerates).

> **Status (2026-06-28).** Implemented (Stages 1-3). **Stage 1**
> (`textDocument/diagnostic`, #350) and **Stage 2** (`workspace/diagnostic`,
> #352) shipped, then **Stage 3** (`result_id` / `Unchanged` caching) landed in
> two parts — document (#358) and workspace (#360) — superseding D7's "`Full`
> every time" first cut. The pure report assembly lives in
> [`crates/lsp/src/pull.rs`](../../crates/lsp/src/pull.rs) (`document_report`,
> `workspace_entry`, `workspace_unchanged`, `diagnostic_result_id`); the shells
> are `handlers/diagnostic.rs` and `handlers/workspace_diagnostic.rs`, dispatched
> from `server.rs` with `diagnostic_provider` advertised
> (`workspace_diagnostics: true`). **Stage 4** (cross-file `#line`
> `related_documents`) remains **deferred** — `related_documents` is still always
> `None` — as does work-done / partial-result streaming. The sections below are
> the original design record, preserved.

## Why pull, and why it's parser-independent

The server today only *pushes* diagnostics: it publishes
`textDocument/publishDiagnostics` on `didOpen`/`didChange`/`didClose`, and the
client maintains a per-URI subscription. That is the wrong shape for an LLM
agent, which wants **request/response**:

- `textDocument/diagnostic` — "what is wrong with this file *right now*?"
- `workspace/diagnostic` — "did my change break anything *anywhere*?" One call
  covering every source file in the workspace's projects, instead of opening
  every file and collecting async pushes.

Pull is a **delivery mechanism**, not a new analysis. It surfaces exactly the
diagnostics the existing lexer + panic-safe parser already produce through
[`diagnostics::grouped_diagnostics`](../../crates/lsp/src/diagnostics.rs) and
[`fsproj_diagnostics::diagnostics_for`](../../crates/lsp/src/fsproj_diagnostics.rs).
It is independent of F#-parser completeness and of sema resolution coverage:
those improve the diagnostics; pull just packages whatever is produced.

## Background: what exists

- **One diagnostic source already.** `server::grouped_for_uri(uri, text,
  &mut Workspace) -> Option<Vec<FileDiagnostics>>` is the single place that
  dispatches by extension (`.fs`/`.fsi`/`.fsx` → lexer+parser diagnostics;
  `.fsproj` → fsproj diagnostics; else → empty) and returns the per-file
  partition. `FileDiagnostics { file: Option<String>, diagnostics }`: `file:
  None` is the document's own group, `file: Some(s)` is a `#line N "s"`
  cross-file relocation.
- **Cross-file relocation is already solved, purely.**
  `publish::resolve_target(generating, file) -> Option<Url>` maps a `#line`
  filename to a URI (absolute as-is; relative against the generating
  document's directory; unresolvable ⇒ dropped). The pull model's
  `relatedDocuments` is the exact analogue of push's cross-file targets.
- **lsp-types 0.95.1 has the full pull API:** `DocumentDiagnosticRequest` /
  `WorkspaceDiagnosticRequest`, the `RelatedFullDocumentDiagnosticReport {
  related_documents, full_document_diagnostic_report }` shape, the
  `WorkspaceFullDocumentDiagnosticReport { uri, version, .. }` shape, and
  `ServerCapabilities.diagnostic_provider:
  Option<DiagnosticServerCapabilities>`.
- **Gap for Stage 2:** `main.rs` parses `InitializeParams` but forwards only
  `capabilities` — `root_uri`/`workspace_folders` are dropped. There is no
  descendant `.fsproj` discovery (only the *ancestor* walk in
  `find_owning_project`). `workspace/diagnostic` needs both.

## Settled decisions

### D1 — One computation, two deliveries
Reuse `grouped_for_uri` verbatim (lift to `pub(crate)`). Push and pull differ
only in *packaging*, never in *what a file's diagnostics are*. No second
diagnostic code path.

### D2 — Pure assembly core, shell does IO
A new `crate::pull` module mirrors `crate::publish`: pure functions turn
`(requested_uri, Vec<FileDiagnostics>)` into the lsp-types report shapes; the
thin `handlers/diagnostic.rs` / `handlers/workspace_diagnostic.rs` do the text
read and project enumeration. `resolve_target` becomes `pub(crate)` and is
shared (parse-don't-validate reuse, exactly as `lexically_normalize` was
extracted to `crate::paths`).

### D3 — Pull is stateless, and same-file only (first cut)
Each request returns a complete `Full` snapshot, so there is **no clearing
bookkeeping** and **no `PublishState`**. A document-pull for `A` reports **only
`A`'s own diagnostics** as `items`: every same-file group, plus any `#line`
group that resolves back to `A` itself (its own contribution, possibly
line-shifted).

`A`'s `#line` groups targeting *other* files are **deferred**, not surfaced as
`related_documents`. The reason is a semantic trap: a client reads
`related_documents[B]` as `B`'s **complete** diagnostic set, but a single
generating document only knows *its own* contribution to `B`, not the union
across every generator (which is exactly what the push planner accumulates).
Emitting one document's slice as a `Full` report for `B` would let two files
that both relocate onto `B` clobber each other, and would never clear `B` when
a generator goes clean. So this cut omits cross-file related documents (the
push path, retained per D4, still delivers them with correct union/clear
semantics); the honest union-based version is the deferred cross-file stage
below. Correctness over availability: omit rather than over-claim.

### D4 — Keep push intact; add pull alongside
A pull-capable client ignores `publishDiagnostics`; there is no conflict and no
regression for existing push clients. Suppressing push when the client
advertises `textDocument.diagnostic` is a **separate, optional follow-up**
(flagged, not built) — it is a behaviour change to the notification path, not
required to ship pull.

### D5 — Overlay-then-disk text
Read a file's text from the `state.docs` open-buffer overlay if present, else
`std::fs::read_to_string` from disk. This lets an agent *write a file on disk
and immediately pull it*, and is mandatory for `workspace/diagnostic` (most
files are not open). The single isolated effect in the shell. An unreadable
URI yields an empty `Full` report (never an error, never a panic).

### D6 — Advertise honestly
`diagnostic_provider = Options(DiagnosticOptions { identifier:
"borzoi", inter_file_dependencies: true, workspace_diagnostics:
<per stage> })`. `inter_file_dependencies: true` is correct: a `#line N "f"`
directive lets an edit in one buffer change `f`'s diagnostics.

### D7 — `Full` every time (first cut)
No `result_id` / `Unchanged` caching initially — spec-compliant and simplest.
Caching is the clearly-scoped Stage 3, built only when a profile says repeated
pulls hurt (leverage compute: measure first; no speculative generality).

### D8 — `workspace/diagnostic` enumerates by Compile membership
Walk the captured workspace roots for `*.fsproj` (skipping
`bin`/`obj`/`.git`/`.direnv`/`node_modules`, not following symlinks so the walk
can't cycle), evaluate each via the cached `Workspace::project`, and emit **one
`Full` report per enumerated file, including empty ones** — an enumerated file
whose text can't be read (a `<Compile>` pointing at a deleted file) still gets an
empty `Full`, never omitted, so the client clears any stale diagnostics for it.
The enumerated set is
each discovered `.fsproj` *itself* (so a broken project file surfaces too) plus
the lexically-normalised Compile `items` across all projects, de-duplicated by
URI (a linked file reported once, under its membership-aware owning project's
defines). Reporting clean files (not just broken ones) is required for stateless
clear-safety — without `result_id`s a client cannot otherwise learn that a
previously-broken file is now clean — and is the honest "here is exactly the set
I checked." `version: None` (the server tracks no document versions). This
delivers a scoped slice of the "workspace-wide project discovery" that
`fsproj-consumption-plan.md` defers (enumeration only — *not* the file→project
ownership index).

### D9 — Pull ≡ Push correctness oracle
For any single document and its `groups`, the `items` pull produces for `A` must
equal what `PublishState::plan` publishes onto `A`'s **own** URI for the same
groups — both represent "`A`'s own diagnostics". (Pull defers `A`'s cross-file
contributions to push per D3, so only the own-URI set is compared.) A shared
property test pins the new path to the trusted one (gospel: reference
implementation / property).

## Functional-core / imperative-shell split

```text
grouped_for_uri (existing shell; reads Workspace)  ──►  Vec<FileDiagnostics>
                                                            │
crate::pull (NEW, pure):                                    ▼
  document_report(requested, groups) -> RelatedFullDocumentDiagnosticReport
        (requested document's own set only; cross-file deferred per D3)
  workspace_entry(uri, groups)       -> Vec<WorkspaceDocumentDiagnosticReport>
        (uses resolve_target; no IO; fully property-testable)

handlers/diagnostic.rs           (shell): read text (overlay/disk) → grouped_for_uri → pull::document_report
handlers/workspace_diagnostic.rs (shell): discover .fsproj → union items → per file: grouped → pull::workspace_entry
```

## Staged implementation

> Each stage on its own branch, stacked as necessary, so a reviewer can review
> each in isolation.

### Stage 1 — `textDocument/diagnostic` (document pull) ✅ DONE

**Dependencies:** none.

**Implements:** D1, D2, D3, D5, D6 (with `workspace_diagnostics: false`), D7.

**Work:**
- New `crate::pull` pure module with `document_report`. Lift
  `publish::resolve_target` and `server::grouped_for_uri` to `pub(crate)`.
- `handlers/diagnostic.rs`: overlay-or-disk read → `grouped_for_uri` →
  `pull::document_report` → `DocumentDiagnosticReportResult::Report(Full(..))`.
  Non-`.fs`/`.fsproj` or unreadable URI → empty `Full`.
- Dispatch arm in `server::handle_request`; advertise `diagnostic_provider`
  (`workspace_diagnostics: false`).

**Correctness oracle:**
- *Pure-core properties:* a same-file group becomes `items`; a self-referential
  `#line` to the document's own URI merges into `items`; a `#line` group
  targeting another file is omitted (no over-claimed `related_documents`).
- *Pull ≡ Push* (D9): the report's `items` equal a fresh `PublishState::plan`'s
  publish onto the document's own URI for the same `(uri, groups)`.
- *Integration:* a buffer with an active-branch lex error → `Full` carries it;
  an on-disk-only file is read and diagnosed (D5); a missing/clean file →
  empty `Full`.

### Stage 2 — `workspace/diagnostic` (workspace pull, full discovery) ✅ DONE

**Dependencies:** Stage 1.

**Implements:** D8, flips D6 to `workspace_diagnostics: true`.

**Work:**
- Capture `root_uri` + `workspace_folders` into `State` at `initialize`
  (extend `main.rs`; add `State::set_workspace_roots`, the pure
  `server::workspace_roots_from_init` doing the `workspaceFolders`-then-`rootUri`
  precedence).
- Descendant `.fsproj` discovery: a bounded, symlink-free recursive walk under
  each root, skipping `bin`/`obj`/`.git`/`.direnv`/`node_modules` (no new
  dependency).
- `handlers/workspace_diagnostic.rs`: discover → evaluate (cached) → enumerate
  each `.fsproj` plus its lexically-normalised Compile `items` → per file
  overlay/disk read → `grouped_for_uri` → one
  `WorkspaceFullDocumentDiagnosticReport` (`version: None`) per file (deduped by
  URI), including empty ones. Each file's report carries its **own** diagnostics
  (cross-file `#line` deferred, as Stage 1).

**Correctness oracle:**
- *Pure `workspace_entry`:* a broken file → `Full` tagged with its URI and
  non-empty items; a clean file → `Full` with empty items.
- *`workspace_roots_from_init`:* `workspaceFolders` win over `rootUri`; empty /
  absent folders fall back to `rootUri`; non-`file:` URIs drop.
- *Discovery unit tests:* finds nested `.fsproj`, skips `obj`.
- *Integration (`tempfile`):* a project with a clean and a broken Compile file →
  exactly the broken one carries a diagnostic, the clean one reports an empty
  `Full`, the `.fsproj` itself is reported; a file linked by two projects
  appears once (dedup); no roots → empty report.

### Stage 3 — `result_id` / `Unchanged` caching ✅ DONE (#358, #360)

**Dependencies:** Stage 1 (document) and/or Stage 2 (workspace).

**Implements:** the `Unchanged` half of the protocol.

**Work (built):** hash a URI's computed diagnostics; when a
request's `previous_result_id` (document) or a matching `PreviousResultId`
(workspace) equals the current hash, return `Unchanged { result_id }` instead
of the full set. Landed as `pull::diagnostic_result_id` (a 128-bit hash of the
exact `(source, symbols)` inputs) consumed by the document handler (#358) and
the workspace handler via `pull::workspace_unchanged` (#360). Work-done /
partial-result streaming for very large `workspace/diagnostic` responses is a
sibling enhancement that remains **deferred**.

### Stage 4 — cross-file `#line` related documents *(deferred; optional)*

**Dependencies:** Stage 1.

**Implements:** the `related_documents` half of `textDocument/diagnostic` that
D3 defers.

**Work (specified, not built now):** when pulling `A`, populate
`related_documents[B]` with `B`'s **full** set — the union over *all* generators
(not just `A`) that `#line`-relocate onto `B`, matching what `PublishState`
publishes. The hard part is enumerating those generators in a stateless pull
(including unopened on-disk generated files such as `obj/Lexer.fs`), and
deciding how `workspace/diagnostic` attributes a generated file's relocated
diagnostics. Until then the push path (D4) remains the correct channel for
`#line` relocation. Build only when a real client needs cross-file diagnostics
over pull rather than push.

## Out of scope

- **Suppressing push for pull-capable clients** (D4) — optional follow-up.
- **`result_id` caching** (Stage 3) — done (#358, #360); work-done /
  partial-result **streaming** remains deferred.
- **`.fsproj` cache file-watch invalidation** — `Workspace.projects` is still
  cached for the server's lifetime, so a `.fsproj` *edited on disk* is not
  re-evaluated within a session (same documented trade-off as
  `fsproj-consumption-plan.md`). Pull faithfully reflects that cache; closing
  it is the orthogonal `didChangeWatchedFiles` work. (Editing `.fs` *content*
  on disk *is* reflected — D5 re-reads it.)
- **A workspace-wide file→project ownership index** — Stage 2 enumerates
  projects for diagnostics; it does not build the ownership index that would
  close consumer #1's non-ancestor linked-file gap.

## Risks

- **`workspace/diagnostic` cost.** Walk + evaluate-all + lex/parse-all is
  O(workspace). Acceptable for occasional agent calls; Stage 3 caching and
  partial-result streaming are the mitigations. Measure before optimising.
- **Stale `.fsproj` cache** (Out of scope) — `define_constants`/`items` stay as
  first seen within a session.
- **Push/pull duplication** for a client using both — harmless (pull-only
  clients ignore pushes); push-suppression deferred (D4).
